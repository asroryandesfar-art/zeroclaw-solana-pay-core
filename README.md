# ZeroClaw Solana Pay — open-source core

A non-custodial command-line tool for Solana Pay. It creates SOL and USDC
payment links and QR codes for your orders and checks on chain whether each
payment arrived. Customers pay from their own wallet straight to your wallet;
the tool only needs your **public** receiving address and never holds funds or
keys.

Supports Solana Mainnet. The payment workflow was tested on Solana Devnet.

- `solpay create-url` — payment link (Solana Pay URL) with a unique reference
- `solpay render-qr` — the same link as a PNG QR code
- `solpay settle` — checks the chain and records `pending`, `paid` or `mismatch`
  exactly once in a local SQLite ledger

No account, token or license key is needed to build or use this core.

## Build

Requirements: Linux or macOS, [rustup](https://rustup.rs), Git.

```sh
git clone https://github.com/asroryandesfar-art/zeroclaw-solana-pay-core.git
cd zeroclaw-solana-pay-core
cargo build --release --locked
cargo test --workspace --locked
./target/release/solpay --help
```

## Try it on Devnet

Devnet is the default and uses free test tokens. Use your own public Devnet
receiving address.

```sh
export SOLANA_CLUSTER=devnet
export SOLANA_RPC_PRIMARY=https://api.devnet.solana.com
export MERCHANT_WALLET='your-public-devnet-address'
mkdir -p -m 700 "$HOME/solpay-devnet"
export INVOICE_DB="$HOME/solpay-devnet/invoices.sqlite"

./target/release/solpay create-url --amount 1 --token USDC --order-id ORDER-1
# copy "url" and "reference" from the JSON output
./target/release/solpay render-qr --url 'PASTE_URL' --out invoice.png
./target/release/solpay settle --reference 'PASTE_REFERENCE'
```

Or run `examples/devnet-quickstart.sh YOUR_PUBLIC_DEVNET_ADDRESS`. Pay the QR
with a Devnet wallet (for example Phantom with Testnet mode on), then run
`settle` again: it reports `paid` only after the payment is verified on chain.

## Mainnet

Mainnet is opt-in and needs your own RPC endpoint, finalized confirmations and
an order ID per invoice. See [MAINNET_CONFIGURATION.md](MAINNET_CONFIGURATION.md).

## Documentation

[API reference](API_REFERENCE.md) · [Configuration](CONFIGURATION.md) ·
[Payment verification](PAYMENT_VERIFICATION.md) · [Testing](TESTING.md) ·
[Backup and restore](BACKUP_RESTORE.md) · [Operations](OPERATIONS_RUNBOOK.md) ·
[Architecture](docs/ARCHITECTURE.md) · [Threat model](docs/THREAT_MODEL.md) ·
[ZeroClaw agent skills](agent/) · [Security](SECURITY.md)

## Official Ubuntu edition

A ready-to-install Ubuntu app built on this core (menu icon, guided activation,
in-app guide) is sold separately as a one-time license:
https://asroryandesfar-art.github.io/zeroclaw-website/

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at
your option. Third-party notices: [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
Independent Solana Pay tool; not affiliated with ZeroClaw Labs.
