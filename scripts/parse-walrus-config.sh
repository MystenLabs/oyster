#!/usr/bin/env bash
set -euo pipefail

# Extracts SUI_RPC_URL (requires two-level file indirection that file_contains
# can't express) and writes it to $PROCMAN_OUTPUT for downstream templates.

CLIENT_CONFIG="$WALRUS_WORKING_DIR/client_config.yaml"
WALLET_CONFIG="$(grep "^wallet_config:" "$CLIENT_CONFIG" | head -1 | sed "s/^wallet_config:[[:space:]]*//")"
SUI_RPC_URL="$(grep 'rpc:' "$WALLET_CONFIG" | head -1 | sed 's/.*rpc:[[:space:]]*//' | tr -d '"')"

echo "SUI_RPC_URL=$SUI_RPC_URL" >> "$PROCMAN_OUTPUT"
echo "Extracted SUI_RPC_URL=$SUI_RPC_URL"
