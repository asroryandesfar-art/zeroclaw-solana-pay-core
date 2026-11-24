#!/usr/bin/env bash
# Verify an invoice's payment status — auto-loads .env, no manual `export` needed.
#
# Usage:
#   scripts/verify.sh                       # verify the LAST invoice from demo.sh
#   scripts/verify.sh <REFERENCE> <AMOUNT_BASE_UNITS> <TOKEN> <ISSUED_AT> <EXPIRES_AT>
#
# This fixes "required configuration 'MERCHANT_WALLET' is not set": that error
# happens when `solpay verify` is run in a shell that never loaded .env. Here we
# source .env first, so MERCHANT_WALLET (and the RPC) are always present.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"
load_env

SOLPAY="$(solpay_bin)"
[[ -n "$SOLPAY" ]] || die "solpay not found — run 'make install' (or 'make build') first"

STATE="$ROOT/agent/data/last_invoice.json"
REF="${1:-}"
AMT="${2:-}"
TOK="${3:-}"
ISSUED_AT="${4:-}"
EXPIRES_AT="${5:-}"

# Fall back to the last invoice created by scripts/demo.sh.
if [[ -z "$REF" && -f "$STATE" ]]; then
  mapfile -t SAVED < <(python3 - "$STATE" <<'PY'
import json, re, sys
with open(sys.argv[1], encoding='utf-8') as f:
    invoice = json.load(f)
reference = invoice.get('reference')
token = invoice.get('token')
amount = invoice.get('amount_base_units')
issued = invoice.get('issued_at_unix')
expires = invoice.get('expires_at_unix')
if not isinstance(reference, str) or not re.fullmatch(r'[1-9A-HJ-NP-Za-km-z]{32,44}', reference):
    sys.exit('invalid saved reference')
if token not in ('SOL', 'USDC') or not all(type(n) is int and n > 0 for n in (amount, issued, expires)):
    sys.exit('invalid saved invoice values')
for value in (reference, amount, token, issued, expires):
    print(value)
PY
  )
  [[ ${#SAVED[@]} -eq 5 ]] || die "saved invoice JSON is invalid"
  REF="${SAVED[0]}"
  AMT="${AMT:-${SAVED[1]}}"
  TOK="${TOK:-${SAVED[2]}}"
  ISSUED_AT="${ISSUED_AT:-${SAVED[3]}}"
  EXPIRES_AT="${EXPIRES_AT:-${SAVED[4]}}"
fi

[[ -n "$REF" ]] || die "no reference given and no saved invoice — run scripts/demo.sh first, or pass a reference"
[[ -n "$AMT" && -n "$TOK" && -n "$ISSUED_AT" && -n "$EXPIRES_AT" ]] || die "amount, token, issuance, and expiry are required for this invoice"
: "${SOLANA_RPC_PRIMARY:=https://api.devnet.solana.com}"
# A unique reference is queried for both assets. Override with SIG_LIMIT.
: "${SIG_LIMIT:=6}"

# Use .env's MERCHANT_WALLET if present; otherwise fall back to the demo wallet.
: "${MERCHANT_WALLET:=$DEMO_MERCHANT_WALLET}"
export MERCHANT_WALLET

echo "verify: token=$TOK reference=$REF amount_base_units=$AMT recipient=$MERCHANT_WALLET"
exec "$SOLPAY" --format human verify \
  --token "$TOK" \
  --reference "$REF" \
  --amount-base-units "$AMT" \
  --issued-at-unix "$ISSUED_AT" \
  --expires-at-unix "$EXPIRES_AT" \
  --recipient "$MERCHANT_WALLET" \
  --signature-limit "$SIG_LIMIT" \
  --rpc "$SOLANA_RPC_PRIMARY"
