//! Black-box CLI integration tests: spawn the real `solpay` binary, feed it
//! flags/environment, and assert the exit code and stdout JSON. These lock the
//! machine-facing contract ZeroClaw depends on. All tests here are hermetic —
//! the only network touched is a refused connection to localhost (RPC-failure
//! case), which is deterministic and fast.

use std::process::Command;

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_solpay");
const WALLET: &str = "9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM";
const REFERENCE: &str = "So11111111111111111111111111111111111111112";

struct Output {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run(args: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.env_clear();
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.args(args).output().expect("failed to spawn solpay");
    Output {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

#[test]
fn create_url_emits_valid_json_and_exits_zero() {
    let o = run(
        &[
            "create-url",
            "--amount",
            "25",
            "--token",
            "USDC",
            "--message",
            "Table 4",
            "--reference",
            REFERENCE,
        ],
        &[("MERCHANT_WALLET", WALLET), ("SOLANA_CLUSTER", "devnet")],
    );
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);

    let v: Value = serde_json::from_str(o.stdout.trim()).expect("stdout is JSON");
    assert_eq!(v["reference"], REFERENCE);
    assert_eq!(v["token"], "USDC");
    assert_eq!(v["cluster"], "devnet");
    assert_eq!(v["amount_base_units"], 25_000_000);
    assert_eq!(v["amount_ui"], "25");
    assert_eq!(v["message"], "Table 4");
    assert!(v["issued_at_unix"].as_u64().unwrap() > 0);
    assert_eq!(
        v["expires_at_unix"].as_u64().unwrap() - v["issued_at_unix"].as_u64().unwrap(),
        900
    );
    assert!(v["url"].as_str().unwrap().starts_with("solana:"));
    assert!(v["url"].as_str().unwrap().contains("spl-token="));
}

#[test]
fn render_qr_writes_a_png_and_exits_zero() {
    let mut out_path = std::env::temp_dir();
    out_path.push(format!("solpay-cli-test-{}.png", std::process::id()));
    let url = "solana:9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM?amount=25&spl-token=4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU&reference=So11111111111111111111111111111111111111112&label=Shop";

    let o = run(
        &[
            "render-qr",
            "--url",
            url,
            "--out",
            out_path.to_str().unwrap(),
        ],
        &[],
    );
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);

    let v: Value = serde_json::from_str(o.stdout.trim()).unwrap();
    assert_eq!(v["format"], "png");
    assert!(v["size_bytes"].as_u64().unwrap() > 0);

    let bytes = std::fs::read(&out_path).expect("png written");
    assert_eq!(&bytes[0..4], &[0x89, 0x50, 0x4E, 0x47], "not a PNG");
    let _ = std::fs::remove_file(&out_path);
}

#[test]
fn unsupported_token_is_invalid_input() {
    let o = run(
        &[
            "create-url",
            "--amount",
            "25",
            "--token",
            "SCAM",
            "--reference",
            REFERENCE,
        ],
        &[("MERCHANT_WALLET", WALLET)],
    );
    assert_eq!(o.code, 2, "stderr: {}", o.stderr);
    assert!(o.stderr.contains("allowlist"));
}

#[test]
fn amount_over_maximum_is_invalid_input() {
    let o = run(
        &[
            "create-url",
            "--amount",
            "5000",
            "--token",
            "USDC",
            "--reference",
            REFERENCE,
        ],
        &[("MERCHANT_WALLET", WALLET), ("MAX_CHARGE", "100")],
    );
    assert_eq!(o.code, 2, "stderr: {}", o.stderr);
    assert!(o.stderr.contains("maximum"));
}

#[test]
fn bad_reference_is_invalid_input() {
    // Endpoints resolve fine; the reference is parsed and rejected before any
    // network call, so this is deterministic and offline.
    let o = run(
        &[
            "verify",
            "--reference",
            "not-a-key",
            "--amount-base-units",
            "25000000",
            "--issued-at-unix",
            "1000",
            "--expires-at-unix",
            "1900",
            "--rpc",
            "https://localhost:1",
        ],
        &[("MERCHANT_WALLET", WALLET)],
    );
    assert_eq!(o.code, 2, "stderr: {}", o.stderr);
}

#[test]
fn zero_amount_cannot_claim_a_payment() {
    let o = run(
        &[
            "verify",
            "--reference",
            REFERENCE,
            "--amount-base-units",
            "0",
            "--issued-at-unix",
            "1000",
            "--expires-at-unix",
            "1900",
            "--rpc",
            "https://localhost:1",
        ],
        &[("MERCHANT_WALLET", WALLET)],
    );
    assert_eq!(o.code, 2, "stderr: {}", o.stderr);
    assert!(o.stderr.contains("greater than zero"));
}

#[test]
fn malformed_rpc_url_does_not_echo_credentials() {
    let secret = "super-secret-query-key";
    let rpc = format!("http://localhost.evil.com/?api_key={secret}");
    let o = run(
        &[
            "verify",
            "--reference",
            REFERENCE,
            "--amount-base-units",
            "25000000",
            "--issued-at-unix",
            "1000",
            "--expires-at-unix",
            "1900",
            "--rpc",
            &rpc,
        ],
        &[("MERCHANT_WALLET", WALLET)],
    );
    assert_eq!(o.code, 3, "stderr: {}", o.stderr);
    assert!(!o.stderr.contains(secret));
}

#[test]
fn noncanonical_usdc_mint_requires_devnet_test_interlock() {
    let mint = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
    let args = [
        "create-url",
        "--amount",
        "25",
        "--token",
        "USDC",
        "--mint",
        mint,
        "--order-id",
        "ORDER_MINT_TEST",
    ];
    let denied = run(
        &args,
        &[("MERCHANT_WALLET", WALLET), ("SOLANA_CLUSTER", "devnet")],
    );
    assert_eq!(denied.code, 3, "stderr: {}", denied.stderr);
    let allowed = run(
        &args,
        &[
            ("MERCHANT_WALLET", WALLET),
            ("SOLANA_CLUSTER", "devnet"),
            ("ALLOW_TEST_MINT_OVERRIDE", "true"),
        ],
    );
    assert_eq!(allowed.code, 0, "stderr: {}", allowed.stderr);
    let mainnet_ledger = std::env::temp_dir().join(format!(
        "solpay-mainnet-mint-{}-{}.sqlite",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mainnet_env = [
        ("MERCHANT_WALLET", WALLET),
        ("SOLANA_CLUSTER", "mainnet-beta"),
        ("ALLOW_MAINNET", "true"),
        ("ALLOW_TEST_MINT_OVERRIDE", "true"),
        ("PAYMENT_COMMITMENT", "finalized"),
        ("MAINNET_RPC_PRIMARY", "https://provider.example/rpc"),
        ("INVOICE_DB", mainnet_ledger.to_str().unwrap()),
    ];
    let mainnet = run(&args, &mainnet_env);
    assert_eq!(
        mainnet.code, 0,
        "canonical mainnet mint is allowed: {}",
        mainnet.stderr
    );
    let wrong_mint = "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU";
    let rejected = run(
        &[
            "create-url",
            "--amount",
            "25",
            "--token",
            "USDC",
            "--mint",
            wrong_mint,
            "--order-id",
            "ORDER_WRONG_MINT",
        ],
        &mainnet_env,
    );
    assert_eq!(rejected.code, 3, "stderr: {}", rejected.stderr);
}

#[test]
fn invoice_is_registered_before_output_and_duplicate_reference_is_rejected() {
    let path = std::env::temp_dir().join(format!(
        "solpay-cli-ledger-{}-{}.sqlite",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let ledger = path.to_str().unwrap();
    let args = [
        "create-url",
        "--amount",
        "25",
        "--token",
        "USDC",
        "--reference",
        REFERENCE,
        "--ledger",
        ledger,
    ];
    let first = run(&args, &[("MERCHANT_WALLET", WALLET)]);
    assert_eq!(first.code, 0, "stderr: {}", first.stderr);
    let output: Value = serde_json::from_str(&first.stdout).unwrap();
    assert_eq!(output["registered"], true);
    let second = run(&args, &[("MERCHANT_WALLET", WALLET)]);
    assert_eq!(second.code, 2, "stderr: {}", second.stderr);
    assert!(second.stderr.contains("reference already exists"));

    let check = run(
        &["settle", "--reference", REFERENCE, "--ledger", ledger],
        &[
            ("MERCHANT_WALLET", WALLET),
            ("SOLANA_RPC_PRIMARY", "http://127.0.0.1:1"),
            ("RPC_MAX_RETRIES", "0"),
            ("RPC_TIMEOUT_MS", "800"),
        ],
    );
    assert_eq!(check.code, 4, "stderr: {}", check.stderr);
}

#[test]
fn backup_and_restore_cli_preserve_invoice_without_overwriting() {
    let directory = std::env::temp_dir().join(format!(
        "solpay-cli-restore-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&directory).unwrap();
    let ledger = directory.join("source.sqlite");
    let backup = directory.join("backup.sqlite");
    let restored = directory.join("restored.sqlite");
    let ledger = ledger.to_str().unwrap();
    let backup = backup.to_str().unwrap();
    let restored = restored.to_str().unwrap();
    let issued = run(
        &[
            "create-url",
            "--amount",
            "1",
            "--token",
            "SOL",
            "--reference",
            REFERENCE,
            "--ledger",
            ledger,
        ],
        &[("MERCHANT_WALLET", WALLET)],
    );
    assert_eq!(issued.code, 0, "{}", issued.stderr);
    let snapshot = run(&["backup-ledger", "--ledger", ledger, "--out", backup], &[]);
    assert_eq!(snapshot.code, 0, "{}", snapshot.stderr);
    let restore = run(
        &["restore-ledger", "--backup", backup, "--out", restored],
        &[],
    );
    assert_eq!(restore.code, 0, "{}", restore.stderr);
    assert_eq!(
        serde_json::from_str::<Value>(&restore.stdout).unwrap()["ledger_path"],
        restored
    );
    let repeat = run(
        &["restore-ledger", "--backup", backup, "--out", restored],
        &[],
    );
    assert_eq!(repeat.code, 5, "{}", repeat.stderr);
    let duplicate = run(
        &[
            "create-url",
            "--amount",
            "1",
            "--token",
            "SOL",
            "--reference",
            REFERENCE,
            "--ledger",
            restored,
        ],
        &[("MERCHANT_WALLET", WALLET)],
    );
    assert_eq!(duplicate.code, 2, "{}", duplicate.stderr);
}

#[test]
fn settlement_and_backup_do_not_recreate_a_missing_ledger() {
    let directory = std::env::temp_dir().join(format!(
        "solpay-missing-ledger-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&directory).unwrap();
    let ledger = directory.join("missing.sqlite");
    let backup = directory.join("backup.sqlite");
    let settled = run(
        &[
            "settle",
            "--reference",
            REFERENCE,
            "--ledger",
            ledger.to_str().unwrap(),
        ],
        &[("MERCHANT_WALLET", WALLET)],
    );
    assert_eq!(settled.code, 5, "{}", settled.stderr);
    let snapshot = run(
        &[
            "backup-ledger",
            "--ledger",
            ledger.to_str().unwrap(),
            "--out",
            backup.to_str().unwrap(),
        ],
        &[],
    );
    assert_eq!(snapshot.code, 5, "{}", snapshot.stderr);
    assert!(!ledger.exists());
    assert!(!backup.exists());
}

#[test]
fn mainnet_without_interlock_is_config_error() {
    let o = run(
        &[
            "create-url",
            "--amount",
            "25",
            "--token",
            "USDC",
            "--reference",
            REFERENCE,
        ],
        &[
            ("MERCHANT_WALLET", WALLET),
            ("SOLANA_CLUSTER", "mainnet-beta"),
        ],
    );
    assert_eq!(o.code, 3, "stderr: {}", o.stderr);
    assert!(o.stderr.contains("ALLOW_MAINNET"));
}

#[test]
fn mainnet_requires_separate_rpc_finality_and_durable_invoice() {
    let base = [
        ("MERCHANT_WALLET", WALLET),
        ("SOLANA_CLUSTER", "mainnet-beta"),
        ("ALLOW_MAINNET", "true"),
    ];
    let args = [
        "create-url",
        "--amount",
        "1",
        "--token",
        "USDC",
        "--order-id",
        "ORDER_MAINNET",
    ];
    let no_finality = run(&args, &base);
    assert_eq!(no_finality.code, 3, "{}", no_finality.stderr);
    assert!(no_finality.stderr.contains("PAYMENT_COMMITMENT"));

    let missing_rpc = run(
        &args,
        &[base.as_slice(), &[("PAYMENT_COMMITMENT", "finalized")]].concat(),
    );
    assert_eq!(missing_rpc.code, 3, "{}", missing_rpc.stderr);
    assert!(missing_rpc.stderr.contains("MAINNET_RPC_PRIMARY"));

    let without_ledger = run(
        &args,
        &[
            base.as_slice(),
            &[
                ("PAYMENT_COMMITMENT", "finalized"),
                ("MAINNET_RPC_PRIMARY", "https://provider.example/rpc"),
            ],
        ]
        .concat(),
    );
    assert_eq!(without_ledger.code, 3, "{}", without_ledger.stderr);
    assert!(without_ledger.stderr.contains("INVOICE_DB"));

    let path = std::env::temp_dir().join(format!(
        "solpay-mainnet-config-{}-{}.sqlite",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let configured = [
        base.as_slice(),
        &[
            ("PAYMENT_COMMITMENT", "finalized"),
            ("MAINNET_RPC_PRIMARY", "https://provider.example/rpc"),
            ("INVOICE_DB", path.to_str().unwrap()),
            ("SOLANA_RPC_PRIMARY", "https://api.devnet.solana.com"),
        ],
    ]
    .concat();
    let issued = run(&args, &configured);
    assert_eq!(issued.code, 0, "{}", issued.stderr);
    let invoice: Value = serde_json::from_str(&issued.stdout).unwrap();
    assert_eq!(invoice["cluster"], "mainnet-beta");
    assert_eq!(invoice["registered"], true);
    assert_eq!(
        invoice["mint"],
        "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
    );

    let reference = invoice["reference"].as_str().unwrap();
    let issued_at = invoice["issued_at_unix"].as_u64().unwrap().to_string();
    let expires_at = invoice["expires_at_unix"].as_u64().unwrap().to_string();
    let verify = [
        "verify",
        "--reference",
        reference,
        "--amount-base-units",
        "1000000",
        "--issued-at-unix",
        &issued_at,
        "--expires-at-unix",
        &expires_at,
        "--rpc",
        "https://api.devnet.solana.com",
    ];
    let override_attempt = run(&verify, &configured);
    assert_eq!(override_attempt.code, 3, "{}", override_attempt.stderr);
    assert!(override_attempt.stderr.contains("overrides"));
}

#[test]
fn rpc_failure_is_transient_exit_four() {
    // Unreachable endpoint (connection refused) -> transient; SOP keeps PENDING.
    let o = run(
        &[
            "verify",
            "--reference",
            REFERENCE,
            "--amount-base-units",
            "25000000",
            "--issued-at-unix",
            "1000",
            "--expires-at-unix",
            "1900",
            "--rpc",
            "https://localhost:1",
        ],
        &[
            ("MERCHANT_WALLET", WALLET),
            ("RPC_MAX_RETRIES", "0"),
            ("RPC_TIMEOUT_MS", "800"),
        ],
    );
    assert_eq!(o.code, 4, "stderr: {}", o.stderr);
    assert!(o.stderr.contains("unavailable"));
}

#[test]
fn help_exits_zero() {
    for args in [
        vec!["--help"],
        vec!["create-url", "--help"],
        vec!["verify", "--help"],
    ] {
        let o = run(&args, &[]);
        assert_eq!(
            o.code, 0,
            "help `{args:?}` should exit 0; stderr: {}",
            o.stderr
        );
        assert!(o.stdout.contains("solpay") || o.stdout.contains("Usage"));
    }
}

/// Live devnet check (ignored by default; run with `--ignored`). A fresh,
/// never-used reference has no signatures, so the verdict is `pending`.
#[test]
#[ignore = "requires network access to Solana devnet"]
fn live_devnet_verify_is_pending_for_fresh_reference() {
    // Generate a fresh reference so getSignaturesForAddress returns empty.
    let created = run(
        &["create-url", "--amount", "25", "--token", "USDC"],
        &[("MERCHANT_WALLET", WALLET), ("SOLANA_CLUSTER", "devnet")],
    );
    assert_eq!(created.code, 0, "stderr: {}", created.stderr);
    let v: Value = serde_json::from_str(created.stdout.trim()).unwrap();
    let reference = v["reference"].as_str().unwrap().to_string();

    let o = run(
        &[
            "verify",
            "--reference",
            &reference,
            "--amount-base-units",
            "25000000",
            "--issued-at-unix",
            &v["issued_at_unix"].to_string(),
            "--expires-at-unix",
            &v["expires_at_unix"].to_string(),
            "--rpc",
            "https://api.devnet.solana.com",
        ],
        &[
            ("MERCHANT_WALLET", WALLET),
            ("RPC_TIMEOUT_MS", "8000"),
            ("RPC_MAX_RETRIES", "2"),
        ],
    );
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    let out: Value = serde_json::from_str(o.stdout.trim()).unwrap();
    assert_eq!(out["status"], "pending");
}
