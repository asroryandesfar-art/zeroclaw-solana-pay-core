# Threat model

See [SECURITY.md](../SECURITY.md) and [ARCHITECTURE.md](../ARCHITECTURE.md) for current controls and open risks.

## Assets and trust boundaries

The helper holds no private keys, but it controls invoice presentation and payment verdicts. A compromised host or agent can redirect a QR to an attacker wallet, falsely fulfill an order, expose RPC or messaging credentials, or deny service. Non-custody limits one class of loss; it does not make the deployment safe from financial harm.

Payment inputs from a message or model are untrusted. Operator configuration and persisted invoice terms must be protected. RPC responses must be checked against the configured cluster, transaction status, reference, recipient, mint, amount, commitment, and block time. The helper still trusts the RPC node's view of chain history rather than cryptographically verifying blocks itself.

## Controls and residual risks

| Threat | Current control | Residual risk |
| --- | --- | --- |
| Fake token or wrong recipient | Canonical USDC mint and derived merchant ATA checks | SPL mint override can intentionally replace the canonical mint; restrict it in production |
| SOL cross-invoice replay | Reference required for SOL and USDC | Wallets omitting SOL reference need manual reconciliation; reference uniqueness needs durable enforcement |
| Incomplete or misleading RPC data | Fail-closed parser, signature cross-check, genesis-hash check, fallback | A malicious matching RPC node can still lie; consider independent confirmation for high-value orders |
| Underpayment, zero amount, and late payment | Integer base units, positive bounded amount, chain blockTime window | Host clock and persisted timestamps must be trustworthy |
| Double settlement / concurrent workers | SQLite ledger has unique reference and signature constraints plus atomic state transitions | ZeroClaw agent runtime use and notification behavior remain unverified |
| Shell argument injection and unauthorized staff | WhatsApp disabled in the buyer template | ZeroClaw v0.8.3 directly interpolates skill arguments into a shell command; replace this bridge before enabling the channel |
| Secret exposure | No private-key path; RPC URL text is redacted from CLI errors | Runtime secret storage and historical Git exposure have not been independently proven |

Use finalized commitment for payments where a confirmed-chain reorganization is unacceptable. A transient RPC error must keep an invoice pending.
