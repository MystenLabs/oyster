#!/usr/bin/env bash
set -euo pipefail

# ---------------------------------------------------------------------------
# Oyster + Pearl local testbed — starts Sui, Walrus, Pearl, Oyster, and the
# extend worker via procman, creates a funded test account, and prints
# connection details.
# ---------------------------------------------------------------------------

# Optional TLS (encryption-only, requires a publicly-trusted cert for Oyster to verify):
#   PEARL_TLS_CERT_PATH=server.crt PEARL_TLS_KEY_PATH=server.key
#   PEARL_GRPC_URL=https://...  (on the Oyster side)

PEARL_BIND_ADDR="127.0.0.1:50051"
OYSTER_BIND_ADDR="127.0.0.1:3000"
PEARL_SERVICE_SECRET="testbed-secret"
# Deterministic 32-byte master seed for local testbed key derivation (NOT for production use).
PEARL_MASTER_SEED="deadbeefcafebabe1234567890abcdef0102030405060708090a0b0c0d0e0f10"
WALRUS_AGGREGATOR_URL="http://127.0.0.1:31415"
FIFO="/tmp/oyster-testbed.fifo"
SUI_FUND_AMOUNT=1000000000       # 1 SUI
WAL_FUND_AMOUNT=500000000000     # 500 WAL (in FROST)
STARTUP_TIMEOUT=60               # seconds
WALRUS_STARTUP_TIMEOUT=300       # seconds (Walrus deploys contracts + starts nodes)

WALRUS_WORKING_DIR="$HOME/src/walrus/working_dir"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

if [[ -d "$WALRUS_WORKING_DIR" ]]; then
  echo "Cleaning existing Walrus working directory at $WALRUS_WORKING_DIR..."
  rm -rf "$WALRUS_WORKING_DIR"
fi

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

usage() {
  cat <<EOF
Usage: $(basename "$0") [OPTIONS]

Start the full Sui + Walrus + Pearl + Oyster stack via procman.

Options:
  --walrus-working-dir <path>  Walrus working directory (default: ~/src/walrus/working_dir)
  --stop                       Kill existing testbed and exit
  --help                       Show this help message

Prerequisites:
  - cargo, sui, walrus, grpcurl, jq, curl, aws, procman, nc in PATH
EOF
}

die() { echo "error: $*" >&2; exit 1; }

check_prereqs() {
  local missing=()
  for cmd in cargo sui walrus grpcurl jq curl aws procman nc; do
    if ! command -v "$cmd" &>/dev/null; then
      missing+=("$cmd")
    fi
  done
  if (( ${#missing[@]} )); then
    die "missing required tools: ${missing[*]}. Install them before running this script."
  fi
}

cleanup() {
  echo "Stopping testbed..."
  procman stop "$FIFO" 2>/dev/null || true

  if [[ -d "$WALRUS_WORKING_DIR" ]]; then
    echo "removing existing Walrus working directory at $WALRUS_WORKING_DIR..."
    rm -rf "$WALRUS_WORKING_DIR"
  fi

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

wait_for_walrus() {
  local client_config="$WALRUS_WORKING_DIR/client_config.yaml"
  echo -n "Waiting for Walrus testbed"
  local elapsed=0
  while (( elapsed < WALRUS_STARTUP_TIMEOUT )); do
    if [[ -f "$client_config" ]] \
      && curl -so /dev/null --connect-timeout 2 "$WALRUS_AGGREGATOR_URL" 2>/dev/null; then
      echo " ready (${elapsed}s)"
      return 0
    fi
    echo -n "."
    sleep 2
    elapsed=$((elapsed + 2))
  done
  echo
  die "Walrus testbed did not become ready within ${WALRUS_STARTUP_TIMEOUT}s"
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
    if curl -so /dev/null --connect-timeout 2 "http://$OYSTER_BIND_ADDR/health" 2>/dev/null; then
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

  # Kill stale testbed and orphaned processes from previous runs.
  cleanup

  # --- Build ---
  echo "Building pearl and oyster..."
  (cd "$REPO_ROOT" && cargo build -p pearl -p oyster)

  # --- Clean stale DBs ---
  rm -f "$REPO_ROOT"/oyster.db*

  # --- Start procman (boots Sui + Walrus from Procfile) ---
  echo "Starting procman (Sui + Walrus)..."
  (cd "$REPO_ROOT" && exec procman serve "$FIFO") &
  PROCMAN_PID=$!
  trap cleanup EXIT

  # --- Wait for Walrus to be ready ---
  wait_for_walrus
  parse_config

  # --- Start Pearl ---
  echo "Starting Pearl..."
  procman start "$FIFO" \
    "PEARL_BIND_ADDR=$PEARL_BIND_ADDR PEARL_SERVICE_SECRET=$PEARL_SERVICE_SECRET PEARL_MASTER_SEED=$PEARL_MASTER_SEED RUST_LOG=info $REPO_ROOT/target/debug/pearl"

  wait_for_pearl

  # --- Create operator Pearl account ---
  echo "Creating operator Pearl account..."
  OPERATOR_ACCOUNT_ID="$(uuidgen | tr '[:upper:]' '[:lower:]')"
  # Proto field is bytes — grpcurl needs base64-encoded raw UUID bytes.
  local operator_id_b64
  operator_id_b64="$(echo -n "$OPERATOR_ACCOUNT_ID" | tr -d '-' | xxd -r -p | base64)"
  local operator_json
  operator_json="$(
    grpcurl -plaintext \
      -import-path "$REPO_ROOT/crates/pearl/proto" -proto pearl.proto \
      -H "Authorization: Bearer $PEARL_SERVICE_SECRET" \
      -d "{\"account_id\": \"$operator_id_b64\"}" \
      "$PEARL_BIND_ADDR" pearl.Pearl/GetAddress
  )"
  OPERATOR_ADDRESS="$(echo "$operator_json" | jq -r '.address')"
  echo "  account_id: $OPERATOR_ACCOUNT_ID"
  echo "  address:    $OPERATOR_ADDRESS"

  # --- Start Oyster ---
  echo "Starting Oyster..."
  procman start "$FIFO" \
    "BIND_ADDR=$OYSTER_BIND_ADDR DATABASE_URL=sqlite:oyster.db?mode=rwc ENABLE_DEBUG=true PEARL_GRPC_URL=http://$PEARL_BIND_ADDR PEARL_SERVICE_SECRET=$PEARL_SERVICE_SECRET SUI_RPC_URL=$SUI_RPC_URL WALRUS_SYSTEM_OBJECT=$WALRUS_SYSTEM_OBJECT WALRUS_STAKING_OBJECT=$WALRUS_STAKING_OBJECT WALRUS_AGGREGATOR_URL=$WALRUS_AGGREGATOR_URL RUST_LOG=info $REPO_ROOT/target/debug/oysterd serve"

  wait_for_oyster

  # --- Create test user ---
  echo "Creating test user account..."
  local user_json
  user_json="$(curl -sf -X POST "http://$OYSTER_BIND_ADDR/api/v1/debug/create-account")"
  USER_ACCOUNT_ID="$(echo "$user_json" | jq -r '.account_id')"
  USER_API_SECRET="$(echo "$user_json" | jq -r '.api_key.secret')"
  echo "  account_id: $USER_ACCOUNT_ID"

  # --- Get user wallet address ---
  echo "Fetching user wallet address..."
  local wallet_json
  wallet_json="$(
    curl -sf -H "Authorization: Bearer $USER_API_SECRET" \
      "http://$OYSTER_BIND_ADDR/api/v1/account/wallet"
  )"
  USER_WALLET="$(echo "$wallet_json" | jq -r '.address')"
  echo "  wallet: $USER_WALLET"

  # --- Create S3 access key ---
  echo "Creating S3 access key..."
  local access_key_json
  access_key_json="$(
    curl -sf -X POST -H "Authorization: Bearer $USER_API_SECRET" \
      "http://$OYSTER_BIND_ADDR/api/v1/account/access-keys"
  )"
  S3_ACCESS_KEY="$(echo "$access_key_json" | jq -r '.access_key_id')"
  S3_SECRET_KEY="$(echo "$access_key_json" | jq -r '.secret_access_key')"
  echo "  access_key_id:     $S3_ACCESS_KEY"
  echo "  secret_access_key: $S3_SECRET_KEY"

  # --- Fund wallets ---
  fund_wallet "$OPERATOR_ADDRESS" "operator wallet"
  echo "Funding user wallet via fund-account.sh..."
  SUI_CLIENT_CONFIG="$WALRUS_WORKING_DIR/sui_admin.yaml" \
    WALRUS_CONFIG="$WALRUS_WORKING_DIR/client_config.yaml" \
    "$REPO_ROOT/scripts/fund-account.sh" \
    "http://$OYSTER_BIND_ADDR" "$USER_API_SECRET" \
    100 sui 500 wal

  # --- Query user wallet balance ---
  local balances_json
  balances_json="$(
    curl -sf -X POST "$SUI_RPC_URL" \
      -H 'Content-Type: application/json' \
      -d "{\"jsonrpc\":\"2.0\",\"method\":\"suix_getAllBalances\",\"params\":[\"$USER_WALLET\"],\"id\":1}"
  )"
  USER_SUI_BALANCE="$(echo "$balances_json" | jq -r '[.result[] | select(.coinType == "0x2::sui::SUI") | .totalBalance | tonumber] | add // 0')"
  USER_WAL_BALANCE="$(echo "$balances_json" | jq -r '[.result[] | select(.coinType | contains("WAL")) | .totalBalance | tonumber] | add // 0')"

  # --- Start Extend worker ---
  echo "Starting Oyster extend worker..."
  procman start "$FIFO" \
    "DATABASE_URL=sqlite:oyster.db?mode=rwc PEARL_GRPC_URL=http://$PEARL_BIND_ADDR PEARL_SERVICE_SECRET=$PEARL_SERVICE_SECRET SUI_RPC_URL=$SUI_RPC_URL WALRUS_SYSTEM_OBJECT=$WALRUS_SYSTEM_OBJECT WALRUS_STAKING_OBJECT=$WALRUS_STAKING_OBJECT OYSTER_EXTENSION_METRICS_BIND_ADDR=127.0.0.1:50053 RUST_LOG=info $REPO_ROOT/target/debug/oysterd extend"

  # --- Configure AWS CLI profile ---
  local aws_profile="oyster-local-testbed"
  echo "Configuring AWS CLI profile '$aws_profile'..."
  aws configure set aws_access_key_id "$S3_ACCESS_KEY" --profile "$aws_profile"
  aws configure set aws_secret_access_key "$S3_SECRET_KEY" --profile "$aws_profile"
  aws configure set region "us-east-1" --profile "$aws_profile"
  aws configure set endpoint_url "http://$OYSTER_BIND_ADDR" --profile "$aws_profile"
  echo "  done"

  # --- Done ---
  cat <<EOF

========================================
 Oyster Local Testbed Ready
========================================
 Oyster URL:       http://$OYSTER_BIND_ADDR
 Bearer Token:     $USER_API_SECRET
 User Wallet:      $USER_WALLET ($((USER_SUI_BALANCE / 1000000000)) SUI, $((USER_WAL_BALANCE / 1000000000)) WAL)
 Operator Wallet:  $OPERATOR_ADDRESS

 S3 Access Key:    $S3_ACCESS_KEY
 S3 Secret Key:    $S3_SECRET_KEY
 S3 Endpoint:      http://$OYSTER_BIND_ADDR

 AWS CLI profile '$aws_profile' configured. Usage:
   aws --profile $aws_profile s3api create-bucket --bucket my-bucket
   aws --profile $aws_profile s3api put-object --bucket my-bucket --key hello.txt --body hello.txt
   aws --profile $aws_profile s3api get-object --bucket my-bucket --key hello.txt out.txt

 Managed by procman. Press Ctrl-C to stop all services.
 Stop:  scripts/local-testbed.sh --stop
========================================
EOF

  # Block until procman exits (Ctrl-C triggers cleanup via EXIT trap).
  wait "$PROCMAN_PID" || true
}

main "$@"
