# CLI reference

This project has **no HTTP API or webhook endpoint**. The supported interface is the `solpay` command. Use `solpay <command> --help` for the full argument list. JSON is the default stdout format; `--format human` is for interactive use. Errors are emitted as structured JSON on stderr.

In the examples, set `REFERENCE`, `ISSUED_AT_UNIX`, and `EXPIRES_AT_UNIX` from the JSON returned by `create-url`; supply a real Solana Pay URL for `render-qr`.

## create-url

~~~sh
solpay create-url --amount 25 --token USDC --message "Order 124"
~~~

Mainnet requires `--order-id` (1–64 ASCII letters, digits, `_`, or `-`) and the configuration in [MAINNET_CONFIGURATION.md](MAINNET_CONFIGURATION.md). The JSON response includes `invoice_id` (the reference) and `order_id`. The ledger rejects a second invoice for the same merchant, cluster, and order ID.

Returns `reference`, `url`, `recipient`, `mint` (null for SOL), `token`, `cluster`, `amount_base_units`, `amount_ui`, `label`, `message`, `issued_at_unix`, `expires_at_unix`, and `registered`. Amounts are decimal input strings; base units are integers in output. With INVOICE_DB or --ledger configured, registered=true means the invoice was committed to SQLite before output. The reference is randomly generated unless explicitly supplied; duplicate references are rejected by the ledger.

## render-qr

~~~sh
solpay render-qr --url 'solana:<recipient>?amount=25&reference=<reference>' --out /tmp/invoice.png
~~~

Returns `image_path`, `format`, `size_bytes`, `modules`, and `pixel_size`.

## verify

~~~sh
solpay verify --reference "$REFERENCE" --amount-base-units 25000000 \
  --token USDC --issued-at-unix "$ISSUED_AT_UNIX" \
  --expires-at-unix "$EXPIRES_AT_UNIX"
~~~

Returns `status` (`paid`, `pending`, or `mismatch`), `signature`, `slot`, and `reason`. A missing transaction, insufficient commitment, missing reference, or missing block time stays pending. A transaction outside the invoice window or with wrong amount, mint, or recipient yields mismatch. RPC uncertainty exits with code 4 and must not settle an order.

## settle

~~~sh
solpay settle --reference "$REFERENCE"
~~~

Requires INVOICE_DB or --ledger. It loads the persisted invoice, verifies its payment, and atomically stores a unique signature. Returns `status`, `order_id`, `signature`, `slot`, `reason`, and `newly_settled`. Notify only when newly_settled=true. A repeated settlement returns paid with newly_settled=false. A signature already claimed by another invoice returns mismatch. RPC uncertainty exits 4 without changing the ledger.

`settle` scans at most `SIGNATURE_SCAN_LIMIT` signatures across bounded pages (default 500, maximum 10000). If a valid payment falls beyond that cap, increase it after assessing RPC cost or reconcile manually.

## backup-ledger

~~~sh
solpay backup-ledger --out /private/backups/invoices-2026-10-11.sqlite
~~~

Creates a consistent SQLite snapshot that includes committed WAL content. The destination must not exist. On Unix, a new backup is owner-private (mode 0600). Restore to a private location while the application is stopped, then verify the restored ledger before resuming settlement.

## restore-ledger

~~~sh
solpay restore-ledger --backup /private/backups/invoices-2026-10-11.sqlite \
  --out /private/restore/invoices.sqlite
~~~

Checks backup integrity and schema, then copies it to a **new** owner-private ledger path. It refuses to overwrite a file, including the active ledger. Stop invoice writers, confirm the command succeeded, verify the restored invoice records, then point `INVOICE_DB` to the new path. A failed or interrupted restore may leave an incomplete destination file; do not activate it. A backup may omit payments made after the snapshot, so reconcile with chain evidence before reopening orders.

## mcp-serve

`solpay mcp-serve` exposes `create_invoice(amount, token, order_id)` and `settle_invoice(reference)` through newline-delimited JSON-RPC over stdio. It reads the owner-private `~/.zeroclaw/solpay.env` as assignments without invoking a shell. This is an internal ZeroClaw adapter, not a network API. The WhatsApp channel is disabled until live routing and authorization are verified.

| Exit code | Meaning |
| --- | --- |
| 0 | Command completed; inspect status in JSON |
| 2 | Invalid caller input |
| 3 | Invalid configuration |
| 4 | RPC or chain evidence unavailable; retry later |
| 5 | Internal failure |

With `INVOICE_DB` configured, `create-url` registers an invoice and `settle` records a successful payment. Neither command sends a webhook or message.
