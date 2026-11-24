#!/bin/bash
# Devnet quick start for the open-source core: creates a 1 USDC invoice,
# saves its QR code and checks its status. No funds are sent.
# usage: examples/devnet-quickstart.sh YOUR_PUBLIC_DEVNET_ADDRESS
set -euo pipefail
cd "$(dirname "$0")/.."
wallet="${1:?usage: examples/devnet-quickstart.sh YOUR_PUBLIC_DEVNET_ADDRESS}"
[ -x target/release/solpay ] || cargo build --release --locked
export SOLANA_CLUSTER=devnet ALLOW_MAINNET=false PAYMENT_COMMITMENT=confirmed
export SOLANA_RPC_PRIMARY="${SOLANA_RPC_PRIMARY:-https://api.devnet.solana.com}"
export MERCHANT_WALLET="$wallet"
mkdir -p -m 700 "$HOME/solpay-devnet"
export INVOICE_DB="$HOME/solpay-devnet/invoices.sqlite"
order="ORDER-$(date +%Y%m%d-%H%M%S)"
out="$(target/release/solpay create-url --amount 1 --token USDC --order-id "$order")"
url="$(printf '%s' "$out" | sed -n 's/.*"url":"\([^"]*\)".*/\1/p')"
reference="$(printf '%s' "$out" | sed -n 's/.*"reference":"\([^"]*\)".*/\1/p')"
target/release/solpay render-qr --url "$url" --out "invoice-$order.png" >/dev/null
echo "Order:      $order (1 USDC on Devnet)"
echo "Link:       $url"
echo "QR code:    invoice-$order.png"
echo "Status:     $(target/release/solpay settle --reference "$reference")"
echo
echo "Pay the QR with a Devnet wallet, then check again:"
echo "  INVOICE_DB=$INVOICE_DB MERCHANT_WALLET=$wallet SOLANA_CLUSTER=devnet SOLANA_RPC_PRIMARY=$SOLANA_RPC_PRIMARY target/release/solpay settle --reference $reference"
