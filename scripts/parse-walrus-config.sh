#!/usr/bin/env bash
set -euo pipefail

# Extracts Walrus config values and writes them to $PROCMAN_OUTPUT
# for downstream processes to reference via ${{ parse-config.KEY }}.

CLIENT_CONFIG="$WALRUS_WORKING_DIR/client_config.yaml"

# Poll for the config file (walrus takes a while to deploy contracts).
echo "Waiting for $CLIENT_CONFIG..."
while [[ ! -f "$CLIENT_CONFIG" ]]; do
  sleep 2
done

yaml_value() {
  grep "^${2}:" "$1" | head -1 | sed "s/^${2}:[[:space:]]*//"
}

SYSTEM_OBJECT="$(yaml_value "$CLIENT_CONFIG" system_object)"
STAKING_OBJECT="$(yaml_value "$CLIENT_CONFIG" staking_object)"
SUI_CLIENT_YAML="$(yaml_value "$CLIENT_CONFIG" wallet_config)"
SUI_RPC_URL="$(grep 'rpc:' "$SUI_CLIENT_YAML" | head -1 | sed 's/.*rpc:[[:space:]]*//' | tr -d '"')"

echo "WALRUS_SYSTEM_OBJECT=$SYSTEM_OBJECT" >> "$PROCMAN_OUTPUT"
echo "WALRUS_STAKING_OBJECT=$STAKING_OBJECT" >> "$PROCMAN_OUTPUT"
echo "SUI_RPC_URL=$SUI_RPC_URL" >> "$PROCMAN_OUTPUT"

echo "Walrus config: system=$SYSTEM_OBJECT staking=$STAKING_OBJECT rpc=$SUI_RPC_URL"
