# Mainnet configuration

Mainnet is opt-in. Keep this configuration in a private environment file or secret manager. Never put provider credentials or any wallet private key in this repository. The merchant wallet is a **public receiving address**; the payer signs in their own wallet.

```sh
SOLANA_CLUSTER=mainnet-beta
ALLOW_MAINNET=true
MERCHANT_WALLET=<merchant-public-wallet>
PAYMENT_COMMITMENT=finalized
MAINNET_RPC_PRIMARY=https://<paid-mainnet-provider-endpoint>
MAINNET_RPC_FALLBACK=https://<independent-mainnet-provider-endpoint>
INVOICE_DB=/private/persistent/mainnet-invoices.sqlite
RPC_TIMEOUT_MS=8000
RPC_MAX_RETRIES=3
RPC_BACKOFF_BASE_MS=250
RPC_MIN_INTERVAL_MS=100
SIGNATURE_SCAN_LIMIT=500
```

Create a protected persistent directory and restrict the environment file and database to the service account. Use a **different** database and provider credentials for devnet. Provision enough RPC capacity for signature pagination, `getTransaction`, genesis checks, and USDC mint checks. A public endpoint is useful for read-only development, not a production capacity guarantee. RPC URLs may contain API keys and are never printed by config errors.

The CLI checks `getGenesisHash` before payment queries and fails closed on a cluster mismatch. Mainnet rejects `--rpc` overrides and requires the separate `MAINNET_RPC_PRIMARY` variable, `PAYMENT_COMMITMENT=finalized`, `ALLOW_MAINNET=true`, `INVOICE_DB`, and `--order-id` at invoice creation. The order ID is unique per merchant and cluster in the ledger; use an internal opaque ID, not a customer phone number. The canonical Mainnet USDC mint is `EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v` with six decimals. The code queries the mint account at finalized commitment and checks its owner, type, and decimals before settlement.

```sh
solpay create-url --amount 0.01 --token SOL --order-id PILOT_001
solpay create-url --amount 0.01 --token USDC --order-id PILOT_002
solpay settle --reference <reference-from-create-url>
```

The commands above are operational examples. **Do not send a Mainnet payment until the owner explicitly approves the controlled pilot and its maximum spend.** Solana transactions incur network fees; an account creation for USDC may also require rent. This repository never constructs or signs a Mainnet payment.

See [payment verification](PAYMENT_VERIFICATION.md), [operations](OPERATIONS_RUNBOOK.md), and the [Solana RPC transaction reference](https://solana.com/docs/rpc/http/gettransaction).
