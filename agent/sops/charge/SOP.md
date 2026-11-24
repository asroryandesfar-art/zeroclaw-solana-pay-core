# Charge

Turns an authorized staff message like `Charge TABLE4 25 USDC` into a persisted Solana Pay invoice and payment URL.
Deterministic: the LLM only extracts intent; recipient/mint/cluster are locked in the
environment and never model-controlled.

## Steps

1. **Authorize sender** — Accept only a configured staff sender; the template has an empty allowlist and a disabled WhatsApp channel until tested live.
2. **Extract intent** — Parse the inbound message into decimal amount, token, and a short ASCII order ID. Reject anything that is not a charge intent. Never produce wallet, mint, cluster, or path fields.
3. **Create invoice** — Call the typed `solpay__create_invoice` MCP tool with only `amount`, `token`, and `order_id`. It returns a reference and a `solana:` URL.
4. **Reply to staff** — Require `registered=true`, then share the payment URL and reference. Do not promise a QR attachment; the agent does not expose path-writing QR rendering.
