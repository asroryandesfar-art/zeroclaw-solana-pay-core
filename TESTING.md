# Testing

## Deterministic checks

~~~sh
cargo fmt --check
cargo check --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --release --locked
cargo audit
~~~

`cargo fmt --all -- --check` is the release formatting gate. Record the exit code of each command; do not count ignored live tests as passes.

`cargo audit` requires the separately installed `cargo-audit` subcommand and an up-to-date RustSec advisory database.

The unit and fixture suites exercise SOL/USDC credit checks, wrong recipient and mint, underpayment, failed and unconfirmed transactions, missing reference, duplicate balance entries, incomplete RPC evidence, wrong cluster, mismatched fetched signature, late payment, pagination boundaries and duplicates, retry/failover, CLI input validation, duplicate invoice references, idempotent settlement, concurrent claims, SQLite migration/backup/restore, and MCP injection handling. RPC transport tests use scripted responses. These tests do not prove that an external RPC node is honest or that a wallet attaches a reference to a native SOL transfer.

One live devnet test is ignored by default. It performs read-only RPC calls for a fresh reference and expects pending:

~~~sh
cargo test --workspace --locked live_devnet_verify_is_pending_for_fresh_reference -- --ignored
~~~

The manual `scripts/devnet_e2e.py` harness creates ephemeral in-memory wallets, requests devnet faucet SOL, then attempts real reference-bearing SOL payments. It requires the optional `solana` Python package. Do not count a blocked faucet as a payment PASS. No test sends a mainnet transaction or transfers real funds. There is no HTTP server, browser frontend, or webhook implementation, so those tests are not applicable. ZeroClaw SOP routing, staff authorization, and WhatsApp delivery remain unverified; the buyer template keeps WhatsApp disabled.

On 2026-10-11, the read-only devnet test passed. The transaction rehearsal stopped before invoice creation because the public faucet did not fund its ephemeral payer. Real SOL and USDC transfer E2E therefore remain blocked or unverified; local mocked-RPC settlement tests cover the payment decisions.

Record compiler version, command output, environment, and skipped checks for each release.

Mainnet read-only coverage includes a captured public version 1 USDC transaction fixture and a fresh invoice smoke that must remain pending. This validates parsing and network access only; it does not prove a ZeroClaw invoice was paid.

The schema v3 regression suite checks that issuing an invoice and claiming a signature append exactly one local audit event each, and that a version 1 paid ledger migrates without losing its claim. The final local workspace gate passed 162 tests with 0 failures and 1 ignored live devnet test on 2026-10-12.

