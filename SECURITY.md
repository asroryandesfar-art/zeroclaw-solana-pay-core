# Security policy

## Reporting a vulnerability

Please **do not** open a public issue for security reports. Instead, report
privately via GitHub Security Advisories ("Report a vulnerability") on this
repository, or contact the maintainers directly.

Include: affected version/commit, a description, reproduction steps, and impact.
We aim to acknowledge within a few days and will coordinate a fix and disclosure.

## Scope and design notes

The Rust helper is **non-custodial**: it holds no customer private key and cannot
sign a transfer. A compromised host can still substitute a merchant address or
payment status, cause loss through false fulfillment, disclose operator secrets,
or deny service. See [ARCHITECTURE.md](ARCHITECTURE.md) for trust boundaries.

Payment verification requires a compiled transfer instruction carrying the
reference as an extra account, the configured cluster's genesis hash, correct
merchant balance increase, canonical mint and merchant-owned associated token
account for USDC, sufficient integer amount, finalized status on Mainnet, and
a block time inside the invoice window. The JSON-RPC envelope is validated;
the RPC provider remains a trusted data source, not a cryptographic proof.
Wallets that omit the reference on native SOL transfers require manual
reconciliation. RPC-based verification is not independent cryptographic block
validation.

The CLI implements a SQLite invoice ledger with unique signature claims and
atomic settlement when INVOICE_DB is configured. It does not send webhooks.
The payment skills use a typed MCP stdio server. The historical ZeroClaw v0.8.3
shell adapter performs unsafe text substitution, so do not reintroduce shell
templates with customer data. The buyer template disables WhatsApp until
sender authorization, actual MCP routing, and delivery are tested live.

`settle` and `backup-ledger` require an existing nonempty SQLite ledger; a
missing database must be treated as storage loss and recovered from a checked
backup. `restore-ledger` writes only to a new path and must not be activated
until the command succeeds and newer chain payments are reconciled.

For local CLI use, keep RPC provider credentials in a git-ignored `.env`.
WhatsApp and LLM credentials belong in the configured ZeroClaw secret store,
whose live encryption behavior must be confirmed before production deployment.
Never commit `.env`, `agent/data/`, `agent/logs/`, private keys, or RPC tokens.
Rotate any secret that was previously committed or exposed in logs.

On Mainnet, keep the primary and fallback RPC credentials in a protected
environment, choose a provider with adequate capacity, and monitor failures.
An unavailable RPC must leave the invoice pending. Late, underpaid, and
reference-free transfers require manual reconciliation; do not discard their
signatures. The CLI has no authenticated HTTP webhook or continuous circuit
breaker, and the disabled WhatsApp template has no live delivery guarantee.
