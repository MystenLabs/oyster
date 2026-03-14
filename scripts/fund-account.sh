#!/usr/bin/env bash
set -euo pipefail

MIST_PER_SUI=1000000000
FROST_PER_WAL=1000000000

die() { echo "error: $*" >&2; exit 1; }

usage() {
  cat <<EOF
Usage: $0 <oyster_base_url> <api_key> <amount> <token> [<amount> <token> ...]

Transfers SUI and/or WAL tokens to an Oyster account's custodial wallet.

Arguments:
  oyster_base_url  Oyster API base URL (e.g. http://localhost:3000)
  api_key          Bearer token for the Oyster API
  amount           Amount in whole tokens (positive integer)
  token            Token type: "sui" or "wal" (case-insensitive)

Environment variables:
  SUI_CLIENT_CONFIG  Path to sui client config (passed as --client.config)
  WALRUS_CONFIG      Path to walrus client config (passed as --config to walrus)
  WALRUS_CONTEXT     Walrus network context (passed as --context to walrus)

Examples:
  $0 http://localhost:3000 abc123... 100 sui
  $0 http://localhost:3000 abc123... 100 sui 200 wal
  SUI_CLIENT_CONFIG=~/.sui/sui_config/client.yaml $0 http://localhost:3000 abc123... 50 wal
EOF
  exit 1
}

check_prereqs() {
  for cmd in "$@"; do
    command -v "$cmd" >/dev/null 2>&1 \
      || die "'$cmd' is required but not found in PATH"
  done
}

sui_client() {
  if [[ -n "${SUI_CLIENT_CONFIG:-}" ]]; then
    sui client --client.config "$SUI_CLIENT_CONFIG" "$@"
  else
    sui client "$@"
  fi
}

# --- Parse args ---
[[ $# -ge 4 ]] || usage

BASE_URL="${1%/}"
API_KEY="$2"
shift 2

(( $# % 2 == 0 )) || die "amount/token arguments must come in pairs"

SUI_AMOUNT=0
WAL_AMOUNT=0

while (( $# )); do
  amount="$1"
  token="${2,,}" # lowercase
  shift 2

  [[ "$amount" =~ ^[1-9][0-9]*$ ]] \
    || die "invalid amount '$amount': must be a positive integer"

  case "$token" in
    sui) SUI_AMOUNT=$((SUI_AMOUNT + amount)) ;;
    wal) WAL_AMOUNT=$((WAL_AMOUNT + amount)) ;;
    *)   die "unknown token '$token': must be 'sui' or 'wal'" ;;
  esac
done

(( SUI_AMOUNT > 0 || WAL_AMOUNT > 0 )) \
  || die "nothing to transfer"

# --- Check prereqs ---
check_prereqs sui jq curl
if (( WAL_AMOUNT > 0 )); then
  check_prereqs walrus
fi

# --- Fetch wallet address ---
echo "Fetching wallet address from $BASE_URL..."
wallet_json="$(
  curl -sf -H "Authorization: Bearer $API_KEY" \
    "$BASE_URL/api/v1/account/wallet"
)" || die "failed to fetch wallet address (is the API key correct?)"

address="$(echo "$wallet_json" | jq -r '.address')"
[[ -n "$address" && "$address" != "null" ]] \
  || die "could not extract wallet address from response"
echo "  wallet: $address"

# --- Transfer SUI ---
if (( SUI_AMOUNT > 0 )); then
  mist=$((SUI_AMOUNT * MIST_PER_SUI))
  echo "Transferring $SUI_AMOUNT SUI ($mist MIST)..."

  sui_coin="$(
    sui_client gas --json | jq -r '.[0].gasCoinId'
  )"
  [[ -n "$sui_coin" && "$sui_coin" != "null" ]] \
    || die "could not find a SUI gas coin in your wallet"
  echo Found SUI coin: "$sui_coin"
  sui_client transfer-sui \
    --to "$address" \
    --sui-coin-object-id "$sui_coin" \
    --amount "$mist" \
    --gas-budget 50000000
  echo "  sent $SUI_AMOUNT SUI ($mist MIST) to $address"
fi

# --- Transfer WAL ---
if (( WAL_AMOUNT > 0 )); then
  frost=$((WAL_AMOUNT * FROST_PER_WAL))
  echo "Transferring $WAL_AMOUNT WAL ($frost FROST)..."

  # Discover WAL coin type. Try `walrus info coin` first, fall back to
  # searching for "WAL" in coinType from `sui client balance`.
  walrus_args=()
  if [[ -n "${WALRUS_CONFIG:-}" ]]; then
    walrus_args+=(--config "$WALRUS_CONFIG")
  fi
  if [[ -n "${WALRUS_CONTEXT:-}" ]]; then
    walrus_args+=(--context "$WALRUS_CONTEXT")
  fi

  wal_coin_type=""
  if wal_coin_type_raw="$(walrus "${walrus_args[@]}" info coin)"; then
    # walrus info coin outputs something like a Sui StructTag — extract it
    wal_coin_type="$(echo "$wal_coin_type_raw" | grep -oE '0x[0-9a-f]+::[a-zA-Z_]+::[a-zA-Z_]+' | head -1 || true)"
  fi

  if [[ -z "$wal_coin_type" ]]; then
    # Fallback: search for "WAL" in coinType from balance output
    wal_coin_type="WAL"
  fi

  wal_coin="$(
    sui_client balance --json \
      | jq -r ".. | objects | select(.coinObjectId? and (.coinType? // \"\" | contains(\"$wal_coin_type\"))) | .coinObjectId" \
      | head -1
  )"
  [[ -n "$wal_coin" && "$wal_coin" != "null" ]] \
    || die "could not find a WAL coin in your wallet"

  sui_client pay \
    --input-coins "$wal_coin" \
    --recipients "$address" \
    --amounts "$frost" \
    --gas-budget 50000000
  echo "  sent $WAL_AMOUNT WAL ($frost FROST) to $address"
fi

# --- Summary ---
echo
echo "Done. Funded $address:"
if (( SUI_AMOUNT > 0 )); then
  echo "  $SUI_AMOUNT SUI ($((SUI_AMOUNT * MIST_PER_SUI)) MIST)"
fi
if (( WAL_AMOUNT > 0 )); then
  echo "  $WAL_AMOUNT WAL ($((WAL_AMOUNT * FROST_PER_WAL)) FROST)"
fi
