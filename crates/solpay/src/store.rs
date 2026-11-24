//! Durable, local invoice state. SQLite transactions and unique constraints
//! provide the settlement guarantees that a pure verdict cannot provide.

use std::path::Path;
use std::time::Duration;

use rusqlite::{params, Connection, ErrorCode, OpenFlags, OptionalExtension, TransactionBehavior};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvoiceRecord {
    pub reference: String,
    pub order_id: Option<String>,
    pub cluster: String,
    pub recipient: String,
    pub token: String,
    pub mint: Option<String>,
    pub amount_base_units: u64,
    pub issued_at_unix: i64,
    pub expires_at_unix: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimOutcome {
    NewlyPaid,
    AlreadyPaid,
}

#[derive(Debug)]
pub enum LedgerError {
    Io(std::io::Error),
    Sql(rusqlite::Error),
    AmountOutOfRange,
    DuplicateReference,
    DuplicateOrder,
    SignatureAlreadyClaimed,
    InvoiceAlreadyPaid,
    UnknownInvoice,
    UnsupportedSchemaVersion,
    Corrupt,
}

impl std::fmt::Display for LedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(_) | Self::Sql(_) => write!(f, "invoice ledger unavailable"),
            Self::AmountOutOfRange => write!(f, "invoice amount exceeds ledger integer range"),
            Self::DuplicateReference => write!(f, "invoice reference already exists"),
            Self::DuplicateOrder => write!(f, "order already has an invoice"),
            Self::SignatureAlreadyClaimed => {
                write!(f, "transaction signature is claimed by another invoice")
            }
            Self::InvoiceAlreadyPaid => write!(f, "invoice is already paid by another transaction"),
            Self::UnknownInvoice => write!(f, "invoice reference not found"),
            Self::UnsupportedSchemaVersion => {
                write!(f, "invoice ledger schema version is unsupported")
            }
            Self::Corrupt => write!(f, "invoice ledger integrity check failed"),
        }
    }
}

impl std::error::Error for LedgerError {}

pub struct PaymentLedger {
    conn: Connection,
}

impl PaymentLedger {
    /// Open a ledger that must already contain data. Settlement and backup
    /// must never silently initialize a replacement after storage loss.
    pub fn open_existing(path: &Path) -> Result<Self, LedgerError> {
        let metadata = std::fs::symlink_metadata(path).map_err(LedgerError::Io)?;
        if !metadata.file_type().is_file() || metadata.len() == 0 {
            return Err(LedgerError::Corrupt);
        }
        Self::open(path)
    }

    pub fn open(path: &Path) -> Result<Self, LedgerError> {
        // SQLite and its journal files must not be world-readable. The parent
        // directory is operator-managed (agent/data is ignored by Git).
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            if let Ok(metadata) = std::fs::symlink_metadata(path) {
                if metadata.file_type().is_symlink() || metadata.permissions().mode() & 0o077 != 0 {
                    return Err(LedgerError::Io(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "invoice ledger must be a private regular file",
                    )));
                }
            }
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(path)
                .map_err(LedgerError::Io)?;
        }
        let conn = Connection::open(path).map_err(LedgerError::Sql)?;
        conn.busy_timeout(Duration::from_secs(5))
            .map_err(LedgerError::Sql)?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")
            .map_err(LedgerError::Sql)?;
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(LedgerError::Sql)?;
        if !(0..=3).contains(&version) {
            return Err(LedgerError::UnsupportedSchemaVersion);
        }
        if version == 0 {
            // Also migrates ledgers created by older revisions of this branch.
            conn.execute_batch(
                "BEGIN IMMEDIATE;
             CREATE TABLE IF NOT EXISTS invoices (
               reference TEXT PRIMARY KEY NOT NULL,
               order_id TEXT,
               cluster TEXT NOT NULL,
               recipient TEXT NOT NULL,
               token TEXT NOT NULL,
               mint TEXT,
               amount_base_units INTEGER NOT NULL CHECK (amount_base_units > 0),
               issued_at_unix INTEGER NOT NULL,
               expires_at_unix INTEGER NOT NULL,
               status TEXT NOT NULL CHECK (status IN ('pending','paid')),
               signature TEXT UNIQUE,
               slot INTEGER,
               CHECK (expires_at_unix > issued_at_unix)
             );
             CREATE UNIQUE INDEX IF NOT EXISTS invoices_signature_unique ON invoices(signature);",
            )
            .map_err(LedgerError::Sql)?;
            let has_order_id = conn
                .prepare("PRAGMA table_info(invoices)")
                .map_err(LedgerError::Sql)?
                .query_map([], |row| row.get::<_, String>(1))
                .map_err(LedgerError::Sql)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(LedgerError::Sql)?
                .iter()
                .any(|name| name == "order_id");
            if !has_order_id {
                conn.execute_batch("ALTER TABLE invoices ADD COLUMN order_id TEXT;")
                    .map_err(LedgerError::Sql)?;
            }
            conn.execute_batch(
                "CREATE UNIQUE INDEX IF NOT EXISTS invoices_order_unique ON invoices(cluster,recipient,order_id) WHERE order_id IS NOT NULL;
                 PRAGMA user_version=2;
                 COMMIT;",
            )
            .map_err(LedgerError::Sql)?;
        } else if version == 1 {
            conn.execute_batch(
                "BEGIN IMMEDIATE;
                 ALTER TABLE invoices ADD COLUMN order_id TEXT;
                 CREATE UNIQUE INDEX invoices_order_unique ON invoices(cluster,recipient,order_id) WHERE order_id IS NOT NULL;
                 PRAGMA user_version=2;
                 COMMIT;",
            )
            .map_err(LedgerError::Sql)?;
        }
        if version <= 2 {
            // Triggers keep invoice and audit event changes in the same SQLite
            // transaction, including claims made by concurrent processes.
            conn.execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS ledger_events (
                   id INTEGER PRIMARY KEY,
                   reference TEXT NOT NULL,
                   event_type TEXT NOT NULL CHECK (event_type IN ('issued','paid','imported_pending','imported_paid')),
                   signature TEXT,
                   occurred_at_unix INTEGER NOT NULL
                 );
                 INSERT INTO ledger_events (reference,event_type,signature,occurred_at_unix)
                   SELECT reference,
                     CASE WHEN status='paid' THEN 'imported_paid' ELSE 'imported_pending' END,
                     signature,issued_at_unix FROM invoices AS invoice
                   WHERE NOT EXISTS (
                     SELECT 1 FROM ledger_events AS event WHERE event.reference=invoice.reference
                   );
                 CREATE TRIGGER IF NOT EXISTS invoices_audit_issue AFTER INSERT ON invoices
                 BEGIN
                   INSERT INTO ledger_events (reference,event_type,signature,occurred_at_unix)
                   VALUES (NEW.reference,'issued',NULL,NEW.issued_at_unix);
                 END;
                 CREATE TRIGGER IF NOT EXISTS invoices_audit_paid AFTER UPDATE OF status ON invoices
                 WHEN OLD.status='pending' AND NEW.status='paid'
                 BEGIN
                   INSERT INTO ledger_events (reference,event_type,signature,occurred_at_unix)
                   VALUES (NEW.reference,'paid',NEW.signature,CAST(strftime('%s','now') AS INTEGER));
                 END;
                 PRAGMA user_version=3;
                 COMMIT;",
            )
            .map_err(LedgerError::Sql)?;
        }
        let integrity: String = conn
            .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
            .map_err(LedgerError::Sql)?;
        if integrity != "ok" {
            return Err(LedgerError::Corrupt);
        }
        Ok(Self { conn })
    }

    /// Create a consistent SQLite snapshot, including committed WAL content.
    /// Refuses to overwrite an existing backup.
    pub fn backup_to(&self, path: &Path) -> Result<(), LedgerError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
                .map_err(LedgerError::Io)?;
        }
        #[cfg(not(unix))]
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(LedgerError::Io)?;
        self.conn
            .backup(
                rusqlite::MAIN_DB,
                path,
                None::<fn(rusqlite::backup::Progress)>,
            )
            .map_err(LedgerError::Sql)
    }

    /// Restore a checked snapshot to a new path. The caller must stop writers
    /// and switch INVOICE_DB only after this returns successfully. Never replace
    /// an existing ledger: it may contain payments newer than the snapshot.
    pub fn restore_from(backup: &Path, out: &Path) -> Result<(), LedgerError> {
        let metadata = std::fs::symlink_metadata(backup).map_err(LedgerError::Io)?;
        if !metadata.file_type().is_file() {
            return Err(LedgerError::Corrupt);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(LedgerError::Corrupt);
            }
        }
        let source = Connection::open_with_flags(backup, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(LedgerError::Sql)?;
        let integrity: String = source
            .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
            .map_err(LedgerError::Sql)?;
        if integrity != "ok" {
            return Err(LedgerError::Corrupt);
        }
        let version: i64 = source
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(LedgerError::Sql)?;
        if !(1..=3).contains(&version) {
            return Err(LedgerError::UnsupportedSchemaVersion);
        }
        source
            .prepare("SELECT reference,cluster,recipient,token,mint,amount_base_units,issued_at_unix,expires_at_unix,status,signature,slot FROM invoices WHERE 0")
            .map_err(LedgerError::Sql)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(out)
                .map_err(LedgerError::Io)?;
        }
        #[cfg(not(unix))]
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(out)
            .map_err(LedgerError::Io)?;
        source
            .backup(
                rusqlite::MAIN_DB,
                out,
                None::<fn(rusqlite::backup::Progress)>,
            )
            .map_err(LedgerError::Sql)?;
        PaymentLedger::open(out)?;
        Ok(())
    }

    pub fn issue(&self, invoice: &InvoiceRecord) -> Result<(), LedgerError> {
        let amount =
            i64::try_from(invoice.amount_base_units).map_err(|_| LedgerError::AmountOutOfRange)?;
        self.conn.execute(
            "INSERT INTO invoices (reference,order_id,cluster,recipient,token,mint,amount_base_units,issued_at_unix,expires_at_unix,status)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,'pending')",
            params![invoice.reference, invoice.order_id, invoice.cluster, invoice.recipient, invoice.token,
                    invoice.mint, amount, invoice.issued_at_unix, invoice.expires_at_unix],
        ).map_err(|e| if is_order_constraint(&e) { LedgerError::DuplicateOrder } else if is_constraint(&e) { LedgerError::DuplicateReference } else { LedgerError::Sql(e) })?;
        Ok(())
    }

    pub fn load(&self, reference: &str) -> Result<InvoiceRecord, LedgerError> {
        self.conn.query_row(
            "SELECT reference,order_id,cluster,recipient,token,mint,amount_base_units,issued_at_unix,expires_at_unix FROM invoices WHERE reference=?1",
            [reference],
            |row| {
                let amount: i64 = row.get(6)?;
                Ok(InvoiceRecord {
                    reference: row.get(0)?, order_id: row.get(1)?, cluster: row.get(2)?, recipient: row.get(3)?,
                    token: row.get(4)?, mint: row.get(5)?, amount_base_units: amount as u64,
                    issued_at_unix: row.get(7)?, expires_at_unix: row.get(8)?,
                })
            },
        ).optional().map_err(LedgerError::Sql)?.ok_or(LedgerError::UnknownInvoice)
    }

    pub fn claimed(&self, reference: &str) -> Result<Option<(String, u64)>, LedgerError> {
        let row: Option<(String, Option<String>, Option<i64>)> = self
            .conn
            .query_row(
                "SELECT status,signature,slot FROM invoices WHERE reference=?1",
                [reference],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(LedgerError::Sql)?;
        match row {
            None => Err(LedgerError::UnknownInvoice),
            Some((status, Some(sig), Some(slot))) if status == "paid" && slot >= 0 => {
                Ok(Some((sig, slot as u64)))
            }
            Some(_) => Ok(None),
        }
    }

    /// Claim a signature and mark its invoice paid in one immediate transaction.
    /// The unique signature index rejects cross-invoice replay across processes.
    pub fn claim(
        &mut self,
        reference: &str,
        signature: &str,
        slot: u64,
    ) -> Result<ClaimOutcome, LedgerError> {
        let slot = i64::try_from(slot).map_err(|_| LedgerError::AmountOutOfRange)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(LedgerError::Sql)?;
        let existing: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT status,signature FROM invoices WHERE reference=?1",
                [reference],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(LedgerError::Sql)?;
        match existing {
            None => return Err(LedgerError::UnknownInvoice),
            Some((status, current)) if status == "paid" => {
                return if current.as_deref() == Some(signature) {
                    Ok(ClaimOutcome::AlreadyPaid)
                } else {
                    Err(LedgerError::InvoiceAlreadyPaid)
                };
            }
            Some(_) => {}
        }
        tx.execute(
            "UPDATE invoices SET status='paid',signature=?1,slot=?2 WHERE reference=?3 AND status='pending'",
            params![signature, slot, reference],
        ).map_err(|e| if is_constraint(&e) { LedgerError::SignatureAlreadyClaimed } else { LedgerError::Sql(e) })?;
        tx.commit().map_err(LedgerError::Sql)?;
        Ok(ClaimOutcome::NewlyPaid)
    }
}

fn is_constraint(error: &rusqlite::Error) -> bool {
    matches!(error, rusqlite::Error::SqliteFailure(e, _) if e.code == ErrorCode::ConstraintViolation)
}

fn is_order_constraint(error: &rusqlite::Error) -> bool {
    matches!(error, rusqlite::Error::SqliteFailure(_, Some(message)) if message.contains("invoices.order_id"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    fn path() -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "solpay-ledger-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        p
    }

    fn invoice(reference: &str) -> InvoiceRecord {
        InvoiceRecord {
            reference: reference.into(),
            order_id: None,
            cluster: "devnet".into(),
            recipient: "merchant".into(),
            token: "USDC".into(),
            mint: Some("mint".into()),
            amount_base_units: 25_000_000,
            issued_at_unix: 1000,
            expires_at_unix: 1900,
        }
    }

    #[test]
    fn issue_is_durable_and_reference_is_unique() {
        let p = path();
        let db = PaymentLedger::open(&p).unwrap();
        db.issue(&invoice("ref1")).unwrap();
        assert!(matches!(
            db.issue(&invoice("ref1")),
            Err(LedgerError::DuplicateReference)
        ));
        drop(db);
        let db = PaymentLedger::open(&p).unwrap();
        assert_eq!(db.load("ref1").unwrap(), invoice("ref1"));
        assert_eq!(db.claimed("ref1").unwrap(), None);
    }

    #[test]
    fn order_id_is_unique_per_merchant_and_cluster() {
        let p = path();
        let db = PaymentLedger::open(&p).unwrap();
        let mut first = invoice("ref-order-1");
        first.order_id = Some("ORDER_123".into());
        db.issue(&first).unwrap();
        let mut duplicate = invoice("ref-order-2");
        duplicate.order_id = first.order_id.clone();
        assert!(matches!(
            db.issue(&duplicate),
            Err(LedgerError::DuplicateOrder)
        ));
        assert_eq!(
            db.load("ref-order-1").unwrap().order_id.as_deref(),
            Some("ORDER_123")
        );
        assert!(matches!(
            db.load("ref-order-2"),
            Err(LedgerError::UnknownInvoice)
        ));
    }

    #[test]
    fn audit_events_follow_issue_and_single_atomic_settlement() {
        let p = path();
        let mut db = PaymentLedger::open(&p).unwrap();
        db.issue(&invoice("audit-ref")).unwrap();
        assert_eq!(
            db.claim("audit-ref", "public-signature", 55).unwrap(),
            ClaimOutcome::NewlyPaid
        );
        assert_eq!(
            db.claim("audit-ref", "public-signature", 55).unwrap(),
            ClaimOutcome::AlreadyPaid
        );
        let mut stmt = db.conn
            .prepare("SELECT event_type,signature FROM ledger_events WHERE reference='audit-ref' ORDER BY id")
            .unwrap();
        let events: Vec<(String, Option<String>)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(
            events,
            vec![
                ("issued".into(), None),
                ("paid".into(), Some("public-signature".into()))
            ]
        );
    }

    #[test]
    fn repeated_claim_is_idempotent_and_signature_is_global_unique() {
        let p = path();
        let mut db = PaymentLedger::open(&p).unwrap();
        db.issue(&invoice("ref1")).unwrap();
        db.issue(&invoice("ref2")).unwrap();
        assert_eq!(
            db.claim("ref1", "sig1", 42).unwrap(),
            ClaimOutcome::NewlyPaid
        );
        assert_eq!(
            db.claim("ref1", "sig1", 42).unwrap(),
            ClaimOutcome::AlreadyPaid
        );
        assert!(matches!(
            db.claim("ref1", "sig2", 43),
            Err(LedgerError::InvoiceAlreadyPaid)
        ));
        assert!(matches!(
            db.claim("ref2", "sig1", 42),
            Err(LedgerError::SignatureAlreadyClaimed)
        ));
        assert_eq!(db.claimed("ref2").unwrap(), None);
    }

    #[test]
    fn concurrent_workers_cannot_claim_one_signature_twice() {
        let p = path();
        let db = PaymentLedger::open(&p).unwrap();
        db.issue(&invoice("ref1")).unwrap();
        db.issue(&invoice("ref2")).unwrap();
        drop(db);
        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = ["ref1", "ref2"]
            .into_iter()
            .map(|reference| {
                let p = p.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let mut db = PaymentLedger::open(&p).unwrap();
                    barrier.wait();
                    db.claim(reference, "same-signature", 42)
                })
            })
            .collect();
        let outcomes: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(
            outcomes
                .iter()
                .filter(|o| matches!(o, Ok(ClaimOutcome::NewlyPaid)))
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|o| matches!(o, Err(LedgerError::SignatureAlreadyClaimed)))
                .count(),
            1
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_world_readable_ledger_file() {
        use std::os::unix::fs::PermissionsExt;
        let p = path();
        std::fs::write(&p, b"").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(PaymentLedger::open(&p), Err(LedgerError::Io(_))));
    }

    #[test]
    fn online_backup_restores_committed_payment_from_wal() {
        let primary = path();
        let backup = path();
        let restored = path();
        let mut db = PaymentLedger::open(&primary).unwrap();
        db.issue(&invoice("paid")).unwrap();
        db.claim("paid", "signature-1", 42).unwrap();
        db.backup_to(&backup).unwrap();
        assert!(matches!(db.backup_to(&backup), Err(LedgerError::Io(_))));
        db.issue(&invoice("later")).unwrap();
        drop(db);

        PaymentLedger::restore_from(&backup, &restored).unwrap();
        assert!(matches!(
            PaymentLedger::restore_from(&backup, &restored),
            Err(LedgerError::Io(_))
        ));
        let mut db = PaymentLedger::open(&restored).unwrap();
        assert_eq!(
            db.claimed("paid").unwrap(),
            Some(("signature-1".into(), 42))
        );
        assert!(matches!(db.load("later"), Err(LedgerError::UnknownInvoice)));
        db.issue(&invoice("new")).unwrap();
        assert!(matches!(
            db.claim("new", "signature-1", 43),
            Err(LedgerError::SignatureAlreadyClaimed)
        ));
    }

    #[test]
    fn restore_rejects_corrupt_backup_without_creating_output() {
        let backup = path();
        let restored = path();
        std::fs::write(&backup, b"not a SQLite backup").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&backup, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert!(PaymentLedger::restore_from(&backup, &restored).is_err());
        assert!(!restored.exists());
    }

    #[test]
    fn schema_version_is_migrated_and_future_version_is_rejected() {
        let p = path();
        let db = PaymentLedger::open(&p).unwrap();
        db.issue(&invoice("legacy")).unwrap();
        db.conn.execute_batch("PRAGMA user_version=0;").unwrap();
        drop(db);
        let db = PaymentLedger::open(&p).unwrap();
        assert_eq!(db.load("legacy").unwrap(), invoice("legacy"));
        let version: i64 = db
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 3);
        db.conn.execute_batch("PRAGMA user_version=4;").unwrap();
        drop(db);
        assert!(matches!(
            PaymentLedger::open(&p),
            Err(LedgerError::UnsupportedSchemaVersion)
        ));
    }

    #[test]
    fn version_one_ledger_migrates_without_losing_paid_invoice() {
        let p = path();
        let old_db = Connection::open(&p).unwrap();
        old_db
            .execute_batch(
                "CREATE TABLE invoices (
               reference TEXT PRIMARY KEY NOT NULL,
               cluster TEXT NOT NULL, recipient TEXT NOT NULL,
               token TEXT NOT NULL, mint TEXT,
               amount_base_units INTEGER NOT NULL CHECK (amount_base_units > 0),
               issued_at_unix INTEGER NOT NULL, expires_at_unix INTEGER NOT NULL,
               status TEXT NOT NULL CHECK (status IN ('pending','paid')),
               signature TEXT UNIQUE, slot INTEGER,
               CHECK (expires_at_unix > issued_at_unix)
             );
             PRAGMA user_version=1;",
            )
            .unwrap();
        let old = invoice("old-ref");
        old_db.execute(
            "INSERT INTO invoices (reference,cluster,recipient,token,mint,amount_base_units,issued_at_unix,expires_at_unix,status,signature,slot) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'paid','old-signature',42)",
            params![old.reference, old.cluster, old.recipient, old.token, old.mint, old.amount_base_units as i64, old.issued_at_unix, old.expires_at_unix],
        ).unwrap();
        drop(old_db);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let migrated = PaymentLedger::open_existing(&p).unwrap();
        assert_eq!(migrated.load("old-ref").unwrap(), old);
        assert_eq!(
            migrated.claimed("old-ref").unwrap(),
            Some(("old-signature".into(), 42))
        );
        let imported: String = migrated
            .conn
            .query_row(
                "SELECT event_type FROM ledger_events WHERE reference='old-ref'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(imported, "imported_paid");
        let mut next = invoice("new-ref");
        next.order_id = Some("NEW_ORDER".into());
        migrated.issue(&next).unwrap();
    }

    #[test]
    fn corrupt_database_is_not_reinitialized_as_empty() {
        let p = path();
        std::fs::write(&p, b"not a SQLite database").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert!(PaymentLedger::open(&p).is_err());
    }

    #[test]
    fn busy_writer_returns_error_then_retry_succeeds() {
        let p = path();
        let first = PaymentLedger::open(&p).unwrap();
        first.issue(&invoice("pending")).unwrap();
        let mut second = PaymentLedger::open(&p).unwrap();
        second.conn.busy_timeout(Duration::from_millis(25)).unwrap();
        first.conn.execute_batch("BEGIN IMMEDIATE;").unwrap();
        assert!(matches!(
            second.claim("pending", "signature-2", 42),
            Err(LedgerError::Sql(_))
        ));
        first.conn.execute_batch("ROLLBACK;").unwrap();
        assert_eq!(
            second.claim("pending", "signature-2", 42).unwrap(),
            ClaimOutcome::NewlyPaid
        );
    }
}
