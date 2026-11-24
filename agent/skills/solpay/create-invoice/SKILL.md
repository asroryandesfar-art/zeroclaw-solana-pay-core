---
name: create-invoice
description: Create a Solana Pay invoice (reference + payment URL) for an amount in an allowed token.
version: 1.0.0
---

# Create Invoice

Turns a validated charge intent into a Solana Pay invoice.

Call the `solpay__create_invoice` MCP tool with `amount`, `token` (`SOL` or
`USDC`), and a short staff `order_id` containing only letters, digits,
underscores, or hyphens. The tool loads merchant wallet, mint, cluster, RPC,
and ledger path from the private operator config. Never accept these fields
from a customer. Only share an invoice when `registered=true`.
