# Changelog

## 0.1.0 — first open-source core release

- `solpay` CLI: Solana Pay links (`create-url`), PNG QR codes (`render-qr`),
  on-chain verification (`verify`) and exactly-once settlement into a local
  SQLite ledger (`settle`), plus ledger backup and restore.
- SOL and canonical USDC; reference-bound evidence checks (recipient, mint,
  amount, finality, invoice window, replay protection).
- Devnet by default; Mainnet behind an explicit interlock with finalized
  commitment, dedicated RPC endpoints and genesis-hash checks.
- ZeroClaw agent skills and SOPs, MCP bridge, threat model and ADRs.
