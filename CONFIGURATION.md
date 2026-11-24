# Configuration

Copy `.env.example` for local CLI use. The typed MCP server reads the owner-private `~/.zeroclaw/solpay.env` created from `agent/solpay.env.example`; it parses known assignments as data and never evaluates them as shell code. Keep RPC provider URLs and WhatsApp/LLM credentials out of Git.

| Setting | Purpose | Default / rule |
| --- | --- | --- |
| `SOLANA_CLUSTER` | Chain identity and canonical USDC mint | `devnet`; `mainnet-beta` needs `ALLOW_MAINNET=true` |
| `MERCHANT_WALLET` | Public receiving wallet | Required, on-curve base58 key |
| `TOKEN_ALLOWLIST` | Allowed symbols | `USDC,SOL` |
| `ALLOW_TEST_MINT_OVERRIDE` | Permit non-canonical SPL test mint | `false`; only works on devnet |
| `MIN_CHARGE`, `MAX_CHARGE` | Decimal charge bounds | `0.01`, `1000`; validated for the chosen asset |
| `INVOICE_TTL_SECONDS` | Validity window returned by `create-url` | `900`; allowed 1–86400 |
| `INVOICE_DB` | Durable SQLite invoice ledger | Required for registered invoices and `settle`; no default |
| `PAYMENT_COMMITMENT` | Chain finality threshold | `confirmed` on devnet; explicit `finalized` required on Mainnet |
| `SOLANA_RPC_PRIMARY` | JSON-RPC HTTPS URL | Required for `verify`; local HTTP allowed for localhost only |
| `SOLANA_RPC_FALLBACK` | Optional backup RPC | Must serve the same genesis hash |
| `RPC_TIMEOUT_MS` | HTTP timeout | `8000` |
| `RPC_MAX_RETRIES` | Extra attempts per endpoint | `3` |
| `RPC_BACKOFF_BASE_MS` | Retry delay before jitter | `250` |
| `SIGNATURE_SCAN_LIMIT` | Maximum signatures inspected by `settle` across pages | `500`; allowed 1–10000; RPC pages are at most 1000 |
| `STORE_LABEL` | Wallet display label | `ZeroClaw Store` |

The CLI rejects `processed` commitment, unsupported clusters, unsafe RPC URLs, off-curve merchant wallets, invalid amounts, and a mint override for native SOL. A non-canonical `--mint` override for SPL requires devnet and `ALLOW_TEST_MINT_OVERRIDE=true`; mainnet rejects it. Do not use test mints for commercial USDC invoices.

Mainnet uses separate `MAINNET_RPC_PRIMARY` and optional `MAINNET_RPC_FALLBACK` rather than the devnet variables or CLI RPC overrides. `RPC_MIN_INTERVAL_MS` spaces calls within one process (Mainnet default 100 ms); `RPC_TIMEOUT_MS`, `RPC_MAX_RETRIES`, and `RPC_BACKOFF_BASE_MS` control bounded failure handling. These controls do not provide a global rate limit across multiple workers. See [Mainnet configuration](MAINNET_CONFIGURATION.md).

With INVOICE_DB set, create-url persists invoice reference and terms before returning registered=true. Keep the parent directory private (mode 0700 on Unix), keep the database and backups on persistent storage, and use `backup-ledger` to include committed WAL state. `restore-ledger` validates a backup and creates a new ledger file without overwriting the active one. Settle loads those fields and claims a signature atomically. The lower-level verify command requires explicit issuance and expiry times and does not load an invoice from storage. Do not let a payer supply expected invoice fields.

The `.env.example` leaves MERCHANT_WALLET blank so direct CLI use fails closed until you set your own public wallet. The demo scripts have a separate devnet fallback. An RPC URL can contain an API key and should be treated as a secret even though error output now redacts URLs.
