# Operations runbook

## Before activation

1. Use a dedicated Mainnet RPC provider plus an independent fallback, private credentials, finalized commitment, and a durable protected SQLite path. Keep devnet paths separate. Check provider rate limits and transaction history retention against the invoice TTL and scan limit.
2. Validate the merchant public wallet and USDC associated token account. Confirm the payer wallet can attach a reference to the actual transfer instruction. Never ask for a customer private key.
3. Run the quality gates in [TESTING.md](TESTING.md), restore drill in [BACKUP_RESTORE.md](BACKUP_RESTORE.md), and a controlled SOL and USDC Mainnet pilot after the owner's explicit approval. Keep WhatsApp disabled until the actual v0.8.3 daemon, sender authorization, MCP routing, and delivery pass end to end.

## Monitoring

Capture counts and latency for invoice creation, pending/paid/mismatch decisions, RPC timeouts/429s, wrong genesis, scan exhaustion, failed mint checks, ledger lock errors, backup age, and unnotified settlements. The current CLI emits structured stdout for each command but has no long-running metrics exporter or durable notification outbox; an operator must provide scheduling, alerting, and one-time delivery controls. Do not log RPC URLs, environment files, phone numbers, private keys, or full customer messages.

## Incident response

- **Wrong network, mint, or merchant address:** stop issuing invoices; keep settlement paused. Inspect configuration and outstanding invoices. Do not change existing invoice terms in SQLite.
- **RPC failure or rate limit:** leave invoices pending; switch only to a vetted endpoint with the same genesis hash and sufficient history. Reconcile recent signatures after recovery. Do not infer nonpayment from a failed query.
- **Database damage or lost storage:** stop writers, preserve files and WAL, restore to a new path from a verified snapshot, then reconcile later on-chain payments before activation.
- **Late, short, or reference-free payment:** retain signature, amount, time, and customer/order context in a protected case log; investigate manually. Do not mark paid automatically or promise a refund from this CLI.
- **Suspected false settlement or leaked RPC token:** pause fulfillment and WhatsApp; preserve the ledger and RPC evidence, rotate the token, inspect provider logs, and notify affected operators.

## Rollback

Disable invoice issuance and fulfillment first. Roll back the binary/config while retaining the ledger; never replace a newer paid ledger with an older backup without reconciliation. The schema v3 migration is forward only, so test rollback against a copy before production deployment. Resume only after a pending/paid consistency review.
