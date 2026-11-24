# Verify Payments

Staff-triggered settlement lookup. The Rust CLI has a SQLite ledger with unique
reference/signature constraints and an atomic settlement transaction. Live
WhatsApp sender authorization and delivery remain unverified.

## Steps

1. **Authorize sender** — Require the configured staff sender allowlist.
2. **Verify on-chain** — For a supplied invoice reference call `solpay__settle_invoice`. The tool loads locked invoice terms from SQLite. MCP error `exit_code=4` keeps the invoice PENDING.
3. **Confirm** — The tool atomically claims a signature and moves PENDING to PAID. Notify staff only when `newly_settled=true`; repeated paid results must not send another confirmation. On mismatch request manual review.
