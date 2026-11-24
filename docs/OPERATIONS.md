# Operations

> The WhatsApp channel is disabled in the buyer template until live MCP routing,
> staff sender authorization, and message delivery are verified.
> These notes apply to the CLI and proposed agent operation only.

Running the assistant in the real world: reliability knobs, the incident
playbook, and the sharp edges worth knowing.

## RPC failover and retries

`verify` tries endpoints in order (`SOLANA_RPC_PRIMARY`, then
`SOLANA_RPC_FALLBACK`), retrying transient failures (timeout, network, HTTP 429
and 5xx) with exponential backoff plus jitter. Non-retryable 4xx moves straight
to the next endpoint. If everything is exhausted, `verify` exits **4** and the
invoice stays **PENDING** — it is retried on the next poll. Configure with
`RPC_TIMEOUT_MS`, `RPC_MAX_RETRIES`, `RPC_BACKOFF_BASE_MS`.

**Recommendation:** use a paid RPC provider for anything beyond a demo; the
public endpoint rate-limits quickly when an invoice's reference is busy.

## Choosing a commitment level

`confirmed` is the default. For high-value invoices consider
`PAYMENT_COMMITMENT=finalized` for stronger finality. Never use `processed` — it is rejected at config time because a
`processed` transaction can still be dropped in a fork.

## Invoice ledger

Set `INVOICE_DB` to persistent storage. `create-url` registers an invoice there
before returning its QR URL. `settle` claims a unique signature under an
immediate SQLite transaction. The ZeroClaw agent's end-to-end use of this path
and its notification behavior still need testing.

## Backups

Invoice state lives at `INVOICE_DB`. Use `solpay backup-ledger --out BACKUP_PATH`
for a consistent online snapshot; copying only the main database file while
WAL is active can omit recent payments. Protect the database and backups as
sensitive business data. `agent/data/` is git-ignored. To rehearse recovery,
stop invoice writers and run `solpay restore-ledger --backup BACKUP_PATH --out
NEW_LEDGER_PATH`. The output path must not exist. Confirm the restored invoices
and reconcile payments since the snapshot before switching `INVOICE_DB`.

## Logs

The CLI emits structured JSON errors on stderr. Retain operator logs securely
and avoid logging full RPC URLs. ZeroClaw audit-log integrity was not verified
by this audit.

## Incident playbook

| Symptom | Likely cause | Action |
|---|---|---|
| Invoices stuck `PENDING` | RPC outage / rate limit | check `SOLANA_RPC_PRIMARY`; add a fallback; confirm exit 4 (not 2/3) |
| `verify` returns `mismatch` | wrong amount / mint / recipient | inspect the `reason`; customer likely underpaid — re-invoice |
| Refuses to start (exit 3) | config error | read stderr; fix the named var (wallet on-curve, https RPC, interlock) |
| "Paid" never sent | confirmation guard already fired, or commitment not reached | check ledger state and commitment level |
| Payment confirmed but reorged | `confirmed` dropped (very rare) | raise to `finalized` for high-value flows |

## Refunds

Out of scope for v1 and, by design, impossible for the agent to perform — it
holds no keys. Refunds are done manually by the merchant from their wallet.

## Monitoring (what to watch)

- backlog of `PENDING` invoices (rising = RPC trouble),
- count of `FAILED` (rising = payment UX or config issue),
- exit-code distribution from `verify` (a spike in 4 = RPC health).
