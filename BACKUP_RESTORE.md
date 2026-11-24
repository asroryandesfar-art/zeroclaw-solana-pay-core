# SQLite backup and restore

`INVOICE_DB` is the authoritative local invoice and signature claim ledger. Place it on persistent storage with private file permissions. Include its backup schedule, retention, encryption, and access control in the operator deployment plan. SQLite WAL content is included by the backup API; copying only the main database file while writers run is unsafe.

```sh
solpay backup-ledger --ledger /private/mainnet-invoices.sqlite --out /private/backups/invoices-20261012.sqlite
solpay restore-ledger --backup /private/backups/invoices-20261012.sqlite --out /private/recovery/invoices.sqlite
```

Backups and restores refuse to overwrite an existing target. Restore checks the backup schema/integrity and creates a new path. Version 1 and 2 ledgers migrate to version 3 on open; imported rows receive a snapshot event, not a fabricated historic issue time. Keep a tested copy before migrating. After restoring, check private permissions, open the restored ledger, and reconcile every payment since the snapshot using finalized chain evidence before changing `INVOICE_DB`. Keep writers and notifications stopped during the switch. An interrupted restore output must never be activated. Verify the procedure on the actual deployment host; the repository only proves local automated recovery tests.
