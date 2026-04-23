#!/usr/bin/env bash
set -euo pipefail

# Testbed setup — runs as a one-shot procman task after oyster is healthy.
# Creates accounts, funds wallets, configures AWS CLI, and prints banner.

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SUI_FUND_AMOUNT=1000000000       # 1 SUI
WAL_FUND_AMOUNT=500000000000     # 500 WAL (in FROST)

die() { echo "error: $*" >&2; exit 1; }

fund_wallet() {
  local address="$1"
  local label="$2"
  local admin_config="$WALRUS_WORKING_DIR/sui_admin.yaml"

  echo "Funding $label ($address)..."

  # --- SUI ---
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

# --- Create operator Pearl account ---
echo "Creating operator Pearl account..."
OPERATOR_ACCOUNT_ID="$(uuidgen | tr '[:upper:]' '[:lower:]')"
operator_id_b64="$(echo -n "$OPERATOR_ACCOUNT_ID" | tr -d '-' | xxd -r -p | base64)"
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

# --- Sign admin JWT ---
INTERNAL_APP_ID="00000000-0000-0000-0000-000000000000"
echo "Signing admin JWT for internal app ($INTERNAL_APP_ID)..."
ADMIN_JWT="$(./target/debug/oysterd app jwt "$INTERNAL_APP_ID")"
echo "  $ADMIN_JWT"

# --- Create test user ---
echo "Creating test user account..."
user_json="$(
  curl -sf -X POST -H "Authorization: Bearer $ADMIN_JWT" \
    "http://$OYSTER_BIND_ADDR/api/v1/accounts"
)"
USER_ACCOUNT_ID="$(echo "$user_json" | jq -r '.account_id')"
USER_API_SECRET="$(echo "$user_json" | jq -r '.api_key.bearer_token')"
echo "  account_id: $USER_ACCOUNT_ID"

# --- Get user wallet address ---
echo "Fetching user wallet address..."
wallet_json="$(
  curl -sf -H "Authorization: Bearer $USER_API_SECRET" \
    "http://$OYSTER_BIND_ADDR/api/v1/account/wallet"
)"
USER_WALLET="$(echo "$wallet_json" | jq -r '.address')"
echo "  wallet: $USER_WALLET"

# --- Create S3 access key ---
echo "Creating S3 access key..."
access_key_json="$(
  curl -sf -X POST -H "Authorization: Bearer $ADMIN_JWT" \
    "http://$OYSTER_BIND_ADDR/api/v1/accounts/$USER_ACCOUNT_ID/access-keys"
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
balances_json="$(
  curl -sf -X POST "$SUI_RPC_URL" \
    -H 'Content-Type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"method\":\"suix_getAllBalances\",\"params\":[\"$USER_WALLET\"],\"id\":1}"
)"
USER_SUI_BALANCE="$(echo "$balances_json" | jq -r '[.result[] | select(.coinType == "0x2::sui::SUI") | .totalBalance | tonumber] | add // 0')"
USER_WAL_BALANCE="$(echo "$balances_json" | jq -r '[.result[] | select(.coinType | contains("WAL")) | .totalBalance | tonumber] | add // 0')"

# --- Configure AWS CLI profile ---
aws_profile="oyster-local-testbed"
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
 Admin JWT:        $ADMIN_JWT
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
 Stop:  procman stop
========================================
EOF
