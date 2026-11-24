# Installation

## Requirements

- Rustup (the repository selects Rust 1.97.1 with rustfmt and Clippy).
- Git and an HTTPS Solana devnet RPC endpoint.
- Optional: ZeroClaw v0.8.3 for the typed MCP adapter. Live WhatsApp operation is not certified.
- Optional: Python 3 for the demo state reader and the separate devnet rehearsal script; the latter also needs `solana` in an isolated environment.

## Build and test locally

~~~sh
git clone https://github.com/asroryandesfar-art/zeroclaw-solana-pay-core.git
cd zeroclaw-solana-pay-core
cp .env.example .env
# Replace MERCHANT_WALLET with your own public receiving address.
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --release --locked
~~~

Create a private directory, uncomment `INVOICE_DB=agent/data/invoices.sqlite` in `.env`, then load the environment when invoking the CLI directly:

~~~sh
mkdir -p agent/data
chmod 700 agent/data
set -a; . ./.env; set +a
target/release/solpay create-url --amount 1 --token USDC
# A successful response must include registered=true.
REFERENCE='paste-reference-from-create-url'
target/release/solpay settle --reference "$REFERENCE"
~~~

The repository's `scripts/demo.sh` and `scripts/verify.sh` source `.env` automatically. Demo payments require a devnet wallet and devnet assets. The Solana Pay URL itself does not force a wallet's network, so confirm the payer wallet is on devnet before scanning a devnet QR. Do not use the shared demo wallet for commercial receipts.

## Operational checks

Run `scripts/verify_env.sh` after configuring the merchant wallet. For a deployment, configure INVOICE_DB on persistent storage and use `solpay backup-ledger --out BACKUP_PATH` for a consistent snapshot. Stop writers and test `solpay restore-ledger --backup BACKUP_PATH --out NEW_LEDGER_PATH` before accepting orders; this never replaces the active ledger.

For ZeroClaw, run `scripts/setup.sh`, edit the owner-private `~/.zeroclaw/solpay.env`, and keep WhatsApp disabled until the typed MCP server connects in your actual ZeroClaw process and sender authorization, delivery, and restart behavior have passed live tests. The installer places the absolute `solpay` binary path in its MCP configuration.

No container image is provided; the CLI is a single Rust binary and no server startup is required.

For Mainnet, follow [MAINNET_CONFIGURATION.md](MAINNET_CONFIGURATION.md): use dedicated RPC capacity, a separate persistent ledger, `PAYMENT_COMMITMENT=finalized`, and a unique `--order-id`. Do not send a pilot transfer without the owner's explicit approval. Run the [backup restore drill](BACKUP_RESTORE.md) and [operations checklist](OPERATIONS_RUNBOOK.md) on the deployment host.
