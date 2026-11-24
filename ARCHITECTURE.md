# Architecture

## Components

| Component | Responsibility | Current evidence |
| --- | --- | --- |
| Rust CLI and library (`crates/solpay`) | Validate amounts and configuration, create transfer URLs, render QR, read Solana RPC, decide payment status | Unit, fixture, and CLI tests |
| ZeroClaw configuration (`agent/`) | WhatsApp channel, MCP bundle, and SOP instructions | Skill/SOP schema validation and MCP process tests; live WhatsApp runtime not verified |
| Typed MCP bridge (`crates/solpay/src/bridge.rs`) | Parse JSON tool inputs, load private operator config, call Rust invoice/settlement functions without a shell | Unit and black-box injection tests |
| SQLite invoice ledger (`crates/solpay/src/store.rs`) | Persist issued terms, atomically claim a unique signature, create online backups, and restore to a new path | Concurrency, migration, busy-writer, and backup/restore tests |
| Notification / webhook delivery | Tell staff or customers that an order settled | Described by SOP; not verified end to end |

There is no HTTP server, browser frontend, or custody service. The Rust helper never signs transactions and requires only the merchant's public receiving wallet. The payer's wallet submits the transfer.

## Payment data flow

~~~text
authorized staff intent -> typed MCP or CLI -> registered URL -> payer wallet
                                                    |
persist invoice terms and unique reference in SQLite v
                                                    Solana network
                                                    |
settle(reference) loads locked terms <- RPC signatures + transaction
           |
  paid / pending / mismatch -> SQLite atomic PENDING -> PAID + signature claim
~~~

With INVOICE_DB configured, create-url persists invoice terms before returning the payment URL. Settle loads those terms, scans bounded signature pages, and performs an atomic PENDING-to-PAID update under unique reference and signature constraints. The lower-level verify command remains evidence-only and accepts explicit fields; do not use it for automatic fulfillment. The ZeroClaw runtime and notification flow still need end-to-end validation.

## Trust boundaries

- Operator configuration controls wallet, cluster, RPC endpoints, mint selection, commitment, and charge limits. The token symbol and amount are validated before URL creation.
- RPC data is parsed fail-closed. The helper checks the endpoint's genesis hash, transaction success, account reference, recipient balance delta, mint, base-unit amount, commitment, and chain block time. It cross-checks the requested signature against the returned transaction signatures.
- This is RPC-based verification, not independent cryptographic re-execution of a Solana block. Use a reputable RPC provider and consider independent node confirmation for high-value payments.
- ZeroClaw v0.8.3's shell skill adapter substitutes arguments without escaping. The payment skills have no shell commands and invoke the typed MCP stdio server. The buyer template still disables WhatsApp because actual sender authorization and delivery need live testing.

## Deployment boundary

Devnet and mainnet-beta have distinct canonical USDC mints and genesis hashes. Testnet is not supported and is rejected as an unknown cluster. Mainnet-beta also requires `ALLOW_MAINNET=true`, finalized commitment, separate Mainnet RPC, durable ledger, and a unique order ID. Verification checks compiled transfer instructions carrying the reference, recipient balance effects, and USDC token account metadata before the atomic SQLite claim. The source includes no private-key management and must remain non-custodial.
