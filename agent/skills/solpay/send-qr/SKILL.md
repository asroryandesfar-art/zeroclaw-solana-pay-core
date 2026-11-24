---
name: send-qr
description: Render a Solana Pay URL to a PNG QR code for sending over WhatsApp.
version: 1.0.0
---

# Send QR

Share the `solana:` payment URL returned by `solpay__create_invoice` only after
`registered=true`. The agent does not expose a path-writing QR tool. An operator
may render the same URL offline with `solpay render-qr --url ... --out ...`.
