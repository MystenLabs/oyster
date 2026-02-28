#!/usr/bin/env bash
set -euo pipefail

# ---------------------------------------------------------------------------
# Oyster + Pearl local testbed — starts both services against a running
# Walrus local testbed, creates a funded test account, and prints connection
# details.
# ---------------------------------------------------------------------------

# Optional TLS (encryption-only, requires a publicly-trusted cert for Oyster to verify):
#   PEARL_TLS_CERT_PATH=server.crt PEARL_TLS_KEY_PATH=server.key
#   PEARL_GRPC_URL=https://...  (on the Oyster side)

PEARL_BIND_ADDR="127.0.0.1:50051"
OYSTER_BIND_ADDR="127.0.0.1:3000"
PEARL_SERVICE_SECRET="testbed-secret"
WALRUS_AGGREGATOR_URL="http://127.0.0.1:31415"
PEARL_TMUX="oyster-testbed-pearl"
OYSTER_TMUX="oyster-testbed-oyster"
SUI_FUND_AMOUNT=1000000000       # 1 SUI
WAL_FUND_AMOUNT=500000000000     # 500 WAL (in FROST)
STARTUP_TIMEOUT=60               # seconds

WALRUS_WORKING_DIR="$HOME/src/walrus/working_dir"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

usage() {
  cat <<EOF
Usage: $(basename "$0") [OPTIONS]

Start the full Oyster + Pearl stack against a running Walrus local testbed.

Options:
  --walrus-working-dir <path>  Walrus working directory (default: ~/src/walrus/working_dir)
  --stop                       Kill existing testbed tmux sessions and exit
  --help                       Show this help message

Prerequisites:
  - Walrus local testbed running (~/src/walrus/scripts/local-testbed.sh -A)
  - cargo, sui, grpcurl, tmux, jq, curl in PATH
EOF
}

die() { echo "error: $*" >&2; exit 1; }

check_prereqs() {
  local missing=()
  for cmd in cargo sui grpcurl tmux jq curl; do
    if ! command -v "$cmd" &>/dev/null; then
      missing+=("$cmd")
    fi
  done
  if (( ${#missing[@]} )); then
    die "missing required tools: ${missing[*]}. Install them before running this script."
  fi
}

kill_port() {
  local port="$1" label="$2"
  local pids
  pids="$(lsof -ti :"$port" 2>/dev/null)" || true
  if [[ -n "$pids" ]]; then
    echo "$pids" | xargs kill 2>/dev/null && echo "  killed $label on port $port" || true
  fi
}

cleanup() {
  echo "Stopping testbed sessions..."
  tmux kill-session -t "$PEARL_TMUX" 2>/dev/null && echo "  killed $PEARL_TMUX session" || true
  tmux kill-session -t "$OYSTER_TMUX" 2>/dev/null && echo "  killed $OYSTER_TMUX session" || true
  kill_port "${PEARL_BIND_ADDR##*:}" "pearl"
  kill_port "${OYSTER_BIND_ADDR##*:}" "oyster"
  echo "Done."
}

# Extract a top-level YAML scalar value (key: value) from a file.
yaml_value() {
  local file="$1" key="$2"
  grep "^${key}:" "$file" | head -1 | sed "s/^${key}:[[:space:]]*//"
}

parse_config() {
  local client_config="$WALRUS_WORKING_DIR/client_config.yaml"
  local admin_config="$WALRUS_WORKING_DIR/sui_admin.yaml"

  [[ -f "$client_config" ]] || die "client_config.yaml not found at $client_config"
  [[ -f "$admin_config" ]] || die "sui_admin.yaml not found at $admin_config"

  WALRUS_SYSTEM_OBJECT="$(yaml_value "$client_config" system_object)"
  WALRUS_STAKING_OBJECT="$(yaml_value "$client_config" staking_object)"
  EXCHANGE_OBJECT="$(grep '^\- ' "$client_config" | head -1 | sed 's/^- //')"

  local sui_client_yaml
  sui_client_yaml="$(yaml_value "$client_config" wallet_config)"
  [[ -f "$sui_client_yaml" ]] || die "sui_client.yaml not found at $sui_client_yaml"
  SUI_RPC_URL="$(grep 'rpc:' "$sui_client_yaml" | head -1 | sed 's/.*rpc:[[:space:]]*//' | tr -d '"')"

  echo "Walrus config:"
  echo "  system_object:  $WALRUS_SYSTEM_OBJECT"
  echo "  staking_object: $WALRUS_STAKING_OBJECT"
  echo "  exchange_object: $EXCHANGE_OBJECT"
  echo "  sui_rpc_url:    $SUI_RPC_URL"
}

wait_for_pearl() {
  echo -n "Waiting for Pearl to start"
  local elapsed=0
  while (( elapsed < STARTUP_TIMEOUT )); do
    # Pearl doesn't support gRPC reflection, so just check TCP connectivity.
    if nc -z "${PEARL_BIND_ADDR%%:*}" "${PEARL_BIND_ADDR##*:}" 2>/dev/null; then
      echo " ready (${elapsed}s)"
      return 0
    fi
    echo -n "."
    sleep 2
    elapsed=$((elapsed + 2))
  done
  echo
  die "Pearl did not start within ${STARTUP_TIMEOUT}s"
}

wait_for_oyster() {
  echo -n "Waiting for Oyster to start"
  local elapsed=0
  while (( elapsed < STARTUP_TIMEOUT )); do
    # /buckets returns 401 without auth, so just check connectivity.
    if curl -so /dev/null --connect-timeout 2 "http://$OYSTER_BIND_ADDR/buckets" 2>/dev/null; then
      echo " ready (${elapsed}s)"
      return 0
    fi
    echo -n "."
    sleep 2
    elapsed=$((elapsed + 2))
  done
  echo
  die "Oyster did not start within ${STARTUP_TIMEOUT}s"
}

fund_wallet() {
  local address="$1"
  local label="$2"
  local admin_config="$WALRUS_WORKING_DIR/sui_admin.yaml"

  echo "Funding $label ($address)..."

  # --- SUI ---
  # Find a SUI coin owned by the admin.
  local admin_sui_coin
  admin_sui_coin="$(
    sui client --client.config "$admin_config" gas --json \
      | jq -r '.[0].gasCoinId'
  )"
  [[ -n "$admin_sui_coin" && "$admin_sui_coin" != "null" ]] \
    || die "could not find admin SUI coin for funding"

  sui client --client.config "$admin_config" \
    transfer-sui --to "$address" \
    --sui-coin-object-id "$admin_sui_coin" \
    --amount "$SUI_FUND_AMOUNT" \
    --gas-budget 50000000 \
    >/dev/null
  echo "  sent $SUI_FUND_AMOUNT MIST ($((SUI_FUND_AMOUNT / 1000000000)) SUI)"

  # --- WAL ---
  local wal_coin
  wal_coin="$(
    sui client --client.config "$admin_config" balance --json \
      | jq -r '.. | objects | select(.coinObjectId? and (.coinType? // "" | contains("WAL"))) | .coinObjectId' \
      | head -1
  )"
  [[ -n "$wal_coin" && "$wal_coin" != "null" ]] \
    || die "could not find admin WAL coin for funding"

  sui client --client.config "$admin_config" \
    pay --input-coins "$wal_coin" \
    --recipients "$address" \
    --amounts "$WAL_FUND_AMOUNT" \
    --gas-budget 50000000 \
    >/dev/null
  echo "  sent $WAL_FUND_AMOUNT FROST ($((WAL_FUND_AMOUNT / 1000000000)) WAL)"
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

main() {
  # --- Parse args ---
  while (( $# )); do
    case "$1" in
      --walrus-working-dir)
        WALRUS_WORKING_DIR="$2"; shift 2 ;;
      --stop)
        cleanup; exit 0 ;;
      --help|-h)
        usage; exit 0 ;;
      *)
        die "unknown option: $1 (try --help)" ;;
    esac
  done

  check_prereqs

  # Verify Walrus testbed is running (aggregator returns 404 on /, so just check connectivity).
  echo "Checking Walrus aggregator at $WALRUS_AGGREGATOR_URL..."
  curl -so /dev/null --connect-timeout 5 "$WALRUS_AGGREGATOR_URL" 2>/dev/null \
    || die "Walrus aggregator not reachable at $WALRUS_AGGREGATOR_URL — start the testbed first"
  echo "  aggregator reachable"

  parse_config

  # Kill stale testbed sessions and orphaned processes from previous runs.
  cleanup

  # --- Build ---
  echo "Building pearl and oyster..."
  (cd "$REPO_ROOT" && cargo build -p pearl -p oyster)

  # --- Clean stale DBs ---
  rm -f "$REPO_ROOT"/pearl.db* "$REPO_ROOT"/oyster.db*

  # --- Start Pearl ---
  echo "Starting Pearl in tmux session '$PEARL_TMUX'..."
  tmux new-session -d -s "$PEARL_TMUX" \
    "cd '$REPO_ROOT' && \
     PEARL_DATABASE_URL='sqlite:pearl.db?mode=rwc' \
     PEARL_BIND_ADDR='$PEARL_BIND_ADDR' \
     PEARL_SERVICE_SECRET='$PEARL_SERVICE_SECRET' \
     RUST_LOG=info \
     cargo run -p pearl; \
     echo 'Pearl exited. Press Enter to close.'; read"

  wait_for_pearl

  # --- Create operator Pearl account ---
  echo "Creating operator Pearl account..."
  local operator_json
  operator_json="$(
    grpcurl -plaintext \
      -import-path "$REPO_ROOT/crates/pearl/proto" -proto pearl.proto \
      -H "Authorization: Bearer $PEARL_SERVICE_SECRET" \
      -d '{}' \
      "$PEARL_BIND_ADDR" pearl.Pearl/CreateAccount
  )"
  OPERATOR_ACCOUNT_ID="$(echo "$operator_json" | jq -r '.accountId')"
  OPERATOR_ADDRESS="$(echo "$operator_json" | jq -r '.address')"
  echo "  account_id: $OPERATOR_ACCOUNT_ID"
  echo "  address:    $OPERATOR_ADDRESS"

  # --- Start Oyster ---
  echo "Starting Oyster in tmux session '$OYSTER_TMUX'..."
  tmux new-session -d -s "$OYSTER_TMUX" \
    "cd '$REPO_ROOT' && \
     BIND_ADDR='$OYSTER_BIND_ADDR' \
     DATABASE_URL='sqlite:oyster.db?mode=rwc' \
     ENABLE_DEBUG=true \
     PEARL_GRPC_URL='http://$PEARL_BIND_ADDR' \
     PEARL_SERVICE_SECRET='$PEARL_SERVICE_SECRET' \
     SUI_RPC_URL='$SUI_RPC_URL' \
     WALRUS_SYSTEM_OBJECT='$WALRUS_SYSTEM_OBJECT' \
     WALRUS_STAKING_OBJECT='$WALRUS_STAKING_OBJECT' \
     WALRUS_AGGREGATOR_URL='$WALRUS_AGGREGATOR_URL' \
     PEARL_ACCOUNT_ID='$OPERATOR_ACCOUNT_ID' \
     RUST_LOG=info \
     cargo run -p oyster -- serve; \
     echo 'Oyster exited. Press Enter to close.'; read"

  wait_for_oyster

  # --- Create test user ---
  echo "Creating test user account..."
  local user_json
  user_json="$(curl -sf -X POST "http://$OYSTER_BIND_ADDR/debug/create-account")"
  USER_ACCOUNT_ID="$(echo "$user_json" | jq -r '.account_id')"
  USER_API_SECRET="$(echo "$user_json" | jq -r '.api_key.secret')"
  echo "  account_id: $USER_ACCOUNT_ID"

  # --- Get user wallet address ---
  echo "Fetching user wallet address..."
  local wallets_json
  wallets_json="$(
    curl -sf -H "Authorization: Bearer $USER_API_SECRET" \
      "http://$OYSTER_BIND_ADDR/account/wallets"
  )"
  USER_WALLET="$(echo "$wallets_json" | jq -r '.wallets[0].address')"
  echo "  wallet: $USER_WALLET"

  # --- Fund wallets ---
  fund_wallet "$OPERATOR_ADDRESS" "operator wallet"
  fund_wallet "$USER_WALLET" "user wallet"

  # --- Done ---
  cat <<EOF

========================================
 Oyster Local Testbed Ready
========================================
 Oyster URL:       http://$OYSTER_BIND_ADDR
 API Key:          $USER_API_SECRET
 User Wallet:      $USER_WALLET
 Operator Wallet:  $OPERATOR_ADDRESS

 tmux sessions:
   Pearl:  tmux attach -t $PEARL_TMUX
   Oyster: tmux attach -t $OYSTER_TMUX

 Stop:  scripts/local-testbed.sh --stop
========================================
EOF
}

main "$@"
