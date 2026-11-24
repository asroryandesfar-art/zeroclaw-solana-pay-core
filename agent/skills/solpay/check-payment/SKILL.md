---
name: check-payment
description: Verify on-chain whether an invoice was paid (paid / pending / mismatch).
version: 1.0.0
---

# Check Payment

The only payment tool that touches the network. Call `solpay__settle_invoice`
with only the invoice reference. It loads the amount, token, timestamps,
recipient, and cluster from the persisted invoice; the model supplies only the
invoice reference. A paid result has `newly_settled=true` only for the worker
that first claimed the signature. Notify staff only on that result.

An MCP error with `exit_code=4` (transient RPC error) is surfaced as "unknown": the caller
must keep the invoice **PENDING**, never flip it to a false PAID/FAILED.
