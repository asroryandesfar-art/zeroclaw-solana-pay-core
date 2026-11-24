//! Command-line interface: argument parsing and command handlers.
//!
//! The default output is machine-readable JSON; `--format human` switches to
//! friendly text. Handlers are thin: they resolve+validate config, call the
//! library, and build an output struct. All fallible steps return [`AppError`]
//! carrying the deterministic exit code.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand, ValueEnum};

use crate::config::{self, PaymentAsset};
use crate::domain::validation::{validate_amount, validate_token};
use crate::error::AppError;
use crate::license::{self, Benefit};
use crate::money::{format_base_units, parse_amount, USDC_DECIMALS};
use crate::output::{
    render, BackupOutput, CreateUrlOutput, OutputFormat, RenderQrOutput, RestoreOutput,
    SettleOutput, VerifyOutput,
};
use crate::qr::{self, DEFAULT_PIXEL_SCALE, DEFAULT_QUIET_ZONE};
use crate::solana::pay_url::{build_transfer_request_url, TransferRequest};
use crate::solana::pubkey::parse_pubkey;
use crate::solana::reference::generate as generate_reference;
use crate::solana::rpc::{RpcClient, UreqTransport};
use crate::solana::verify::{verify_payment, Expected, ExpectedAsset, PaymentWindow, Verdict};
use crate::store::{ClaimOutcome, InvoiceRecord, LedgerError, PaymentLedger};

#[derive(Parser, Debug)]
#[command(
    name = "solpay",
    version,
    about = "Non-custodial Solana Pay helper: create SOL/USDC requests, render QR, verify chain evidence, and settle invoices in a local ledger."
)]
pub struct Cli {
    /// Output format. `json` (default) is machine-readable; `human` is text.
    #[arg(long, value_enum, default_value_t = FormatArg::Json, global = true)]
    pub format: FormatArg,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum FormatArg {
    Json,
    Human,
}

impl From<FormatArg> for OutputFormat {
    fn from(f: FormatArg) -> Self {
        match f {
            FormatArg::Json => OutputFormat::Json,
            FormatArg::Human => OutputFormat::Human,
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Verify a signed ZSP1 license and its selected entitlement.
    LicenseCheck(LicenseCheckArgs),

    /// Show the pinned issuer fingerprint of a commercial-package binary.
    #[cfg(feature = "commercial-package")]
    LicenseIssuer,
    /// Build an invoice's reference and Solana Pay URL (stateless, offline).
    #[command(
        after_help = "EXAMPLE:\n  MERCHANT_WALLET=<wallet> SOLANA_CLUSTER=devnet \\\n    solpay create-url --amount 25 --token USDC --message \"Table 4\""
    )]
    CreateUrl(CreateUrlArgs),

    /// Render a Solana Pay URL to a PNG QR code.
    #[command(
        after_help = "EXAMPLE:\n  solpay render-qr --url 'solana:<recipient>?amount=25&spl-token=<mint>&reference=<ref>&label=Shop' \\\n    --out /tmp/qr.png"
    )]
    RenderQr(RenderQrArgs),

    /// Verify on-chain whether an invoice has been paid.
    #[command(
        after_help = "EXAMPLE:\n  SOLANA_RPC_PRIMARY=https://api.devnet.solana.com MERCHANT_WALLET=<wallet> \\\n    solpay verify --reference <ref> --amount-base-units 25000000"
    )]
    Verify(VerifyArgs),

    /// Verify a persisted invoice and atomically claim a payment signature.
    Settle(SettleArgs),

    /// Create a consistent, private SQLite backup without copying an active WAL.
    BackupLedger(BackupLedgerArgs),

    /// Restore a checked backup to a new path without replacing an existing ledger.
    RestoreLedger(RestoreLedgerArgs),

    /// Serve typed payment tools over MCP stdio for ZeroClaw (no shell).
    McpServe(McpServeArgs),
}

#[derive(clap::Args, Debug)]
pub struct LicenseCheckArgs {
    /// Owner-private license key file. Defaults to the activated license.
    #[arg(long)]
    pub license_file: Option<PathBuf>,
    /// Trusted issuer PEM; required for the open-source diagnostic binary.
    #[arg(long)]
    pub issuer_public_key: Option<PathBuf>,
    /// Use is perpetual; updates and support have separate deadlines.
    #[arg(long, value_enum, default_value_t = Benefit::Use)]
    pub benefit: Benefit,
}

#[derive(clap::Args, Debug)]
pub struct CreateUrlArgs {
    /// Human amount, e.g. "25" or "0.5".
    #[arg(long)]
    pub amount: String,
    /// Token symbol (must be in the allowlist), e.g. "USDC".
    #[arg(long)]
    pub token: String,
    /// Merchant order identifier, unique per merchant and cluster in the ledger.
    #[arg(long)]
    pub order_id: Option<String>,
    /// Reuse a specific reference (base58). If omitted, a secure one is generated.
    #[arg(long)]
    pub reference: Option<String>,
    /// Label shown in wallets. Defaults to STORE_LABEL.
    #[arg(long)]
    pub label: Option<String>,
    /// Free-text message/memo, e.g. "Table 4".
    #[arg(long)]
    pub message: Option<String>,
    /// Merchant wallet (locked). Defaults to MERCHANT_WALLET.
    #[arg(long)]
    pub recipient: Option<String>,
    /// Token mint override (locked). Defaults to the per-cluster mint.
    #[arg(long)]
    pub mint: Option<String>,
    /// Cluster (locked). Defaults to SOLANA_CLUSTER.
    #[arg(long)]
    pub cluster: Option<String>,
    /// SQLite invoice ledger path; defaults to INVOICE_DB when set.
    #[arg(long)]
    pub ledger: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct SettleArgs {
    /// Reference of an invoice already persisted in the ledger.
    #[arg(long)]
    pub reference: String,
    /// SQLite invoice ledger path; defaults to INVOICE_DB.
    #[arg(long)]
    pub ledger: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct BackupLedgerArgs {
    /// New backup path; an existing file is never overwritten.
    #[arg(long)]
    pub out: PathBuf,
    /// SQLite invoice ledger path; defaults to INVOICE_DB.
    #[arg(long)]
    pub ledger: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct RestoreLedgerArgs {
    /// Existing owner-private SQLite backup.
    #[arg(long)]
    pub backup: PathBuf,
    /// New ledger path; an existing file is never overwritten.
    #[arg(long)]
    pub out: PathBuf,
}

#[derive(clap::Args, Debug)]
pub struct McpServeArgs {
    /// Private operator-owned config file; defaults to ~/.zeroclaw/solpay.env.
    #[arg(long)]
    pub env_file: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct RenderQrArgs {
    /// The Solana Pay URL to encode (must start with `solana:`).
    #[arg(long)]
    pub url: String,
    /// Output PNG path.
    #[arg(long)]
    pub out: PathBuf,
    /// Pixels per QR module.
    #[arg(long)]
    pub scale: Option<u32>,
    /// White border in modules.
    #[arg(long)]
    pub quiet_zone: Option<u32>,
}

#[derive(clap::Args, Debug)]
pub struct VerifyArgs {
    /// The invoice's reference (base58).
    #[arg(long)]
    pub reference: String,
    /// Exact expected amount in base units.
    #[arg(long)]
    pub amount_base_units: u64,
    /// Issuance time returned by create-url; persist it with the invoice.
    #[arg(long)]
    pub issued_at_unix: i64,
    /// Expiry time returned by create-url; persist it with the invoice.
    #[arg(long)]
    pub expires_at_unix: i64,
    /// Invoice token: USDC (default) or SOL.
    #[arg(long)]
    pub token: Option<String>,
    /// Merchant wallet (locked). Defaults to MERCHANT_WALLET.
    #[arg(long)]
    pub recipient: Option<String>,
    /// Token mint override (locked). Defaults to the per-cluster USDC mint.
    #[arg(long)]
    pub mint: Option<String>,
    /// Cluster (locked). Defaults to SOLANA_CLUSTER.
    #[arg(long)]
    pub cluster: Option<String>,
    /// Commitment threshold: `confirmed` or `finalized`. Defaults to PAYMENT_COMMITMENT.
    #[arg(long)]
    pub commitment: Option<String>,
    /// Primary RPC URL (locked). Defaults to SOLANA_RPC_PRIMARY.
    #[arg(long)]
    pub rpc: Option<String>,
    /// Fallback RPC URL (locked). Defaults to SOLANA_RPC_FALLBACK.
    #[arg(long)]
    pub rpc_fallback: Option<String>,
    /// Max signatures to inspect for the reference.
    #[arg(long)]
    pub signature_limit: Option<u32>,
}

/// Parse args and run, returning the rendered output string on success.
pub fn dispatch(command: Command, format: OutputFormat) -> Result<String, AppError> {
    match command {
        Command::LicenseCheck(a) => run_license_check(a),
        #[cfg(feature = "commercial-package")]
        Command::LicenseIssuer => Ok(serde_json::json!({"issuerFingerprint": license::issuer_fingerprint(license::embedded_issuer()).map_err(AppError::config)?}).to_string()),
        Command::CreateUrl(a) => Ok(render(&run_create_url(a)?, format)),
        Command::RenderQr(a) => Ok(render(&run_render_qr(a)?, format)),
        Command::Verify(a) => Ok(render(&run_verify(a)?, format)),
        Command::Settle(a) => Ok(render(&run_settle(a)?, format)),
        Command::BackupLedger(a) => Ok(render(&run_backup_ledger(a)?, format)),
        Command::RestoreLedger(a) => Ok(render(&run_restore_ledger(a)?, format)),
        Command::McpServe(_) => Err(AppError::internal("mcp-serve requires stdio dispatcher")),
    }
}

fn run_license_check(a: LicenseCheckArgs) -> Result<String, AppError> {
    let path = match a.license_file {
        Some(path) if path.is_absolute() => path,
        Some(_) => return Err(AppError::config("license file path must be absolute")),
        None => license::installed_license_path().map_err(AppError::config)?,
    };
    let key = license::read_private_license(&path).map_err(AppError::config)?;
    #[cfg(feature = "commercial-package")]
    let issuer = {
        if a.issuer_public_key.is_some() {
            return Err(AppError::config(
                "issuer override disabled in commercial package",
            ));
        }
        license::embedded_issuer().to_string()
    };
    #[cfg(not(feature = "commercial-package"))]
    let issuer = {
        let path = a.issuer_public_key.ok_or_else(|| {
            AppError::config("--issuer-public-key required for diagnostic verification")
        })?;
        std::fs::read_to_string(path)
            .map_err(|_| AppError::config("issuer public key unreadable"))?
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AppError::config("system clock invalid"))?
        .as_millis() as i64;
    let status = license::verify(&key, &issuer, now, a.benefit).map_err(AppError::config)?;
    serde_json::to_string(&status)
        .map_err(|_| AppError::internal("license status serialization failed"))
}

pub(crate) fn run_create_url(a: CreateUrlArgs) -> Result<CreateUrlOutput, AppError> {
    let cluster = config::cluster_from(a.cluster.as_deref())?;
    if let Some(order_id) = a.order_id.as_deref() {
        if order_id.is_empty()
            || order_id.len() > 64
            || !order_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        {
            return Err(AppError::invalid_input(
                "order_id must be 1-64 ASCII letters, digits, underscores, or hyphens",
            ));
        }
    }
    if cluster == config::Cluster::MainnetBeta {
        if a.order_id.is_none() {
            return Err(AppError::config(
                "--order-id is required for mainnet invoices",
            ));
        }
        config::commitment_for_cluster(cluster, None)?;
        config::rpc_endpoints_for_cluster(cluster, None, None)?;
        if ledger_path(a.ledger.clone()).is_none() {
            return Err(AppError::config(
                "INVOICE_DB is required for mainnet invoices",
            ));
        }
    }
    let recipient = config::recipient_from(a.recipient.as_deref())?;

    // The token symbol comes from the user/message, so an unsupported token is
    // *invalid input* (exit 2) — validate it here, before the config-level asset
    // resolution, so it is never misclassified as a config error (exit 3).
    validate_token(&a.token, &config::allowlist_from_env())?;
    let asset = config::asset_from(&a.token, cluster, a.mint.as_deref())?;
    let decimals = asset.decimals();

    let amount_base = parse_amount(&a.amount, decimals)?;
    let limits = config::charge_limits_from_env(decimals)?;
    validate_amount(amount_base, &limits)?;
    let amount_ui = format_base_units(amount_base, decimals);
    let ttl = config::u64_from_env("INVOICE_TTL_SECONDS", 900)?;
    if !(1..=86_400).contains(&ttl) {
        return Err(AppError::invalid_input(
            "INVOICE_TTL_SECONDS must be between 1 and 86400",
        ));
    }
    let issued_at_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| AppError::internal(e.to_string()))?
        .as_secs();
    let expires_at_unix = issued_at_unix
        .checked_add(ttl)
        .ok_or_else(|| AppError::internal("invoice expiry overflow"))?;

    // Reuse a provided reference (deterministic) or generate a secure one.
    let reference = match a.reference.as_deref() {
        Some(r) => parse_pubkey(r)?,
        None => generate_reference().map_err(|e| AppError::internal(e.to_string()))?,
    };

    // `None` mint = native SOL (URL omits spl-token); `Some` = SPL/USDC.
    let mint = match &asset {
        PaymentAsset::Sol => None,
        PaymentAsset::Spl { mint } => Some(mint),
    };

    let label = config::label_from(a.label.as_deref());
    let url = build_transfer_request_url(&TransferRequest {
        recipient: &recipient,
        amount_ui: &amount_ui,
        spl_token: mint,
        reference: &reference,
        label: &label,
        message: a.message.as_deref(),
    });

    let registered = if let Some(path) = ledger_path(a.ledger) {
        let ledger = PaymentLedger::open(&path).map_err(map_ledger_error)?;
        let invoice = InvoiceRecord {
            reference: reference.to_string(),
            order_id: a.order_id.clone(),
            cluster: cluster.as_str().to_string(),
            recipient: recipient.to_string(),
            token: crate::domain::validation::normalize_symbol(&a.token),
            mint: mint.map(|m| m.to_string()),
            amount_base_units: amount_base,
            issued_at_unix: i64::try_from(issued_at_unix)
                .map_err(|_| AppError::internal("invoice time out of range"))?,
            expires_at_unix: i64::try_from(expires_at_unix)
                .map_err(|_| AppError::internal("invoice time out of range"))?,
        };
        ledger.issue(&invoice).map_err(map_ledger_error)?;
        true
    } else {
        false
    };

    Ok(CreateUrlOutput {
        invoice_id: reference.to_string(),
        order_id: a.order_id,
        reference: reference.to_string(),
        url,
        recipient: recipient.to_string(),
        mint: mint.map(|m| m.to_string()),
        token: crate::domain::validation::normalize_symbol(&a.token),
        cluster: cluster.as_str().to_string(),
        amount_base_units: amount_base,
        amount_ui,
        label,
        message: a.message,
        issued_at_unix,
        expires_at_unix,
        registered,
    })
}

fn run_render_qr(a: RenderQrArgs) -> Result<RenderQrOutput, AppError> {
    let scale = a.scale.unwrap_or(DEFAULT_PIXEL_SCALE);
    let quiet = a.quiet_zone.unwrap_or(DEFAULT_QUIET_ZONE);
    let info = qr::render_png(&a.url, &a.out, scale, quiet).map_err(map_qr_error)?;
    Ok(RenderQrOutput {
        image_path: a.out.display().to_string(),
        format: "png".to_string(),
        size_bytes: info.size_bytes,
        modules: info.modules,
        pixel_size: info.pixel_size,
    })
}

fn map_qr_error(e: qr::QrError) -> AppError {
    match e {
        // Input problems (bad scheme / data too long) are invalid input.
        qr::QrError::InvalidScheme | qr::QrError::Encode(_) => {
            AppError::invalid_input(e.to_string())
        }
        qr::QrError::Io(_) => AppError::internal(e.to_string()),
    }
}

fn run_verify(a: VerifyArgs) -> Result<VerifyOutput, AppError> {
    let cluster = config::cluster_from(a.cluster.as_deref())?;
    let recipient = config::recipient_from(a.recipient.as_deref())?;
    let token = a.token.as_deref().unwrap_or("USDC");
    validate_token(token, &config::allowlist_from_env())?;
    let asset = config::asset_from(token, cluster, a.mint.as_deref())?;
    let limits = config::charge_limits_from_env(asset.decimals())?;
    validate_amount(a.amount_base_units, &limits)?;
    let commitment = config::commitment_for_cluster(cluster, a.commitment.as_deref())?;
    let endpoints =
        config::rpc_endpoints_for_cluster(cluster, a.rpc.as_deref(), a.rpc_fallback.as_deref())?;
    let reference = parse_pubkey(&a.reference)?;
    let window = PaymentWindow::new(a.issued_at_unix, a.expires_at_unix)
        .ok_or_else(|| AppError::invalid_input("invalid invoice issuance/expiry times"))?;

    let timeout = Duration::from_millis(config::u64_from_env("RPC_TIMEOUT_MS", 8_000)?);
    let retries = config::u32_from_env("RPC_MAX_RETRIES", 3)?;
    let backoff = Duration::from_millis(config::u64_from_env("RPC_BACKOFF_BASE_MS", 250)?);
    let limit = a.signature_limit.unwrap_or(20);
    if !(1..=10_000).contains(&limit) {
        return Err(AppError::invalid_input(
            "signature limit must be between 1 and 10000",
        ));
    }

    // Build the expectation for the invoice's asset: SPL (USDC → merchant ATA)
    // or native SOL (lamports → merchant wallet).
    let expected = match asset {
        PaymentAsset::Spl { mint } => {
            Expected::new(reference, mint, recipient, a.amount_base_units, commitment)
                .with_payment_window(window)
        }
        PaymentAsset::Sol => {
            Expected::new_sol(reference, recipient, a.amount_base_units, commitment)
                .with_payment_window(window)
        }
    };
    let client = RpcClient::new(UreqTransport::new(timeout), endpoints, retries, backoff)
        .with_genesis_hash(cluster.genesis_hash())
        .with_min_interval(rpc_min_interval(cluster)?);
    if cluster == config::Cluster::MainnetBeta {
        if let ExpectedAsset::Spl { mint, .. } = &expected.asset {
            client.verify_legacy_mint(mint, USDC_DECIMALS)?;
        }
    }

    let verdict = verify_payment(&client, &expected, limit)?;
    Ok(match verdict {
        Verdict::Paid { signature, slot } => VerifyOutput::paid(signature, slot),
        Verdict::Mismatch { reason } => VerifyOutput::mismatch(reason),
        Verdict::Pending => VerifyOutput::pending(),
    })
}

fn ledger_path(flag: Option<PathBuf>) -> Option<PathBuf> {
    flag.or_else(|| {
        std::env::var_os("INVOICE_DB")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    })
}

fn rpc_min_interval(cluster: config::Cluster) -> Result<Duration, AppError> {
    let default = if cluster == config::Cluster::MainnetBeta {
        100
    } else {
        0
    };
    let millis = config::u64_from_env("RPC_MIN_INTERVAL_MS", default)?;
    if millis > 5_000 {
        return Err(AppError::config("RPC_MIN_INTERVAL_MS must be at most 5000"));
    }
    Ok(Duration::from_millis(millis))
}

fn map_ledger_error(error: LedgerError) -> AppError {
    match error {
        LedgerError::UnknownInvoice
        | LedgerError::DuplicateReference
        | LedgerError::DuplicateOrder => AppError::invalid_input(error.to_string()),
        _ => AppError::internal(error.to_string()),
    }
}

fn run_backup_ledger(a: BackupLedgerArgs) -> Result<BackupOutput, AppError> {
    let path = ledger_path(a.ledger)
        .ok_or_else(|| AppError::config("INVOICE_DB is required for backup-ledger"))?;
    let ledger = PaymentLedger::open_existing(&path).map_err(map_ledger_error)?;
    ledger.backup_to(&a.out).map_err(map_ledger_error)?;
    let size_bytes = std::fs::metadata(&a.out)
        .map_err(|_| AppError::internal("could not inspect completed backup"))?
        .len();
    Ok(BackupOutput {
        backup_path: a.out.display().to_string(),
        size_bytes,
    })
}

fn run_restore_ledger(a: RestoreLedgerArgs) -> Result<RestoreOutput, AppError> {
    PaymentLedger::restore_from(&a.backup, &a.out).map_err(map_ledger_error)?;
    Ok(RestoreOutput {
        ledger_path: a.out.display().to_string(),
    })
}

pub(crate) fn run_settle(a: SettleArgs) -> Result<SettleOutput, AppError> {
    let path = ledger_path(a.ledger)
        .ok_or_else(|| AppError::config("INVOICE_DB is required for settle"))?;
    let mut ledger = PaymentLedger::open_existing(&path).map_err(map_ledger_error)?;
    let reference = parse_pubkey(&a.reference)?;
    let invoice = ledger
        .load(&reference.to_string())
        .map_err(map_ledger_error)?;
    let cluster = config::cluster_from_env()?;
    let recipient = config::recipient_from(None)?;
    if invoice.cluster != cluster.as_str() || invoice.recipient != recipient.to_string() {
        return Err(AppError::config(
            "persisted invoice cluster or recipient differs from active configuration",
        ));
    }
    if let Some((signature, slot)) = ledger
        .claimed(&invoice.reference)
        .map_err(map_ledger_error)?
    {
        return Ok(SettleOutput {
            status: "paid".into(),
            order_id: invoice.order_id.clone(),
            signature: Some(signature),
            slot: Some(slot),
            reason: None,
            newly_settled: false,
        });
    }
    let asset = config::asset_from(&invoice.token, cluster, invoice.mint.as_deref())?;
    // The amount was validated when the invoice was issued. A later operator
    // change to charge limits must not invalidate an already-issued invoice.
    if invoice.amount_base_units == 0 {
        return Err(AppError::internal("persisted invoice has invalid amount"));
    }
    let window = PaymentWindow::new(invoice.issued_at_unix, invoice.expires_at_unix)
        .ok_or_else(|| AppError::internal("persisted invoice has invalid timestamps"))?;
    let commitment = config::commitment_for_cluster(cluster, None)?;
    let expected = match asset {
        PaymentAsset::Sol => {
            Expected::new_sol(reference, recipient, invoice.amount_base_units, commitment)
        }
        PaymentAsset::Spl { mint } => Expected::new(
            reference,
            mint,
            recipient,
            invoice.amount_base_units,
            commitment,
        ),
    }
    .with_payment_window(window);
    let endpoints = config::rpc_endpoints_for_cluster(cluster, None, None)?;
    let timeout = Duration::from_millis(config::u64_from_env("RPC_TIMEOUT_MS", 8_000)?);
    let retries = config::u32_from_env("RPC_MAX_RETRIES", 3)?;
    let backoff = Duration::from_millis(config::u64_from_env("RPC_BACKOFF_BASE_MS", 250)?);
    let client = RpcClient::new(UreqTransport::new(timeout), endpoints, retries, backoff)
        .with_genesis_hash(cluster.genesis_hash())
        .with_min_interval(rpc_min_interval(cluster)?);
    if cluster == config::Cluster::MainnetBeta {
        if let ExpectedAsset::Spl { mint, .. } = &expected.asset {
            client.verify_legacy_mint(mint, USDC_DECIMALS)?;
        }
    }
    let signature_limit = config::u32_from_env("SIGNATURE_SCAN_LIMIT", 500)?;
    if !(1..=10_000).contains(&signature_limit) {
        return Err(AppError::config(
            "SIGNATURE_SCAN_LIMIT must be between 1 and 10000",
        ));
    }
    match verify_payment(&client, &expected, signature_limit)? {
        Verdict::Paid { signature, slot } => {
            match ledger.claim(&invoice.reference, &signature, slot) {
                Ok(outcome) => Ok(SettleOutput {
                    status: "paid".into(),
                    order_id: invoice.order_id.clone(),
                    signature: Some(signature),
                    slot: Some(slot),
                    reason: None,
                    newly_settled: outcome == ClaimOutcome::NewlyPaid,
                }),
                Err(LedgerError::SignatureAlreadyClaimed) => Ok(SettleOutput {
                    status: "mismatch".into(),
                    order_id: invoice.order_id.clone(),
                    signature: None,
                    slot: None,
                    reason: Some("transaction signature is claimed by another invoice".into()),
                    newly_settled: false,
                }),
                Err(e) => Err(map_ledger_error(e)),
            }
        }
        Verdict::Mismatch { reason } => Ok(SettleOutput {
            status: "mismatch".into(),
            order_id: invoice.order_id.clone(),
            signature: None,
            slot: None,
            reason: Some(reason),
            newly_settled: false,
        }),
        Verdict::Pending => Ok(SettleOutput {
            status: "pending".into(),
            order_id: invoice.order_id.clone(),
            signature: None,
            slot: None,
            reason: None,
            newly_settled: false,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses_and_verifies_argument_structure() {
        // clap's own derive invariants (no conflicting flags, valid structure).
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
