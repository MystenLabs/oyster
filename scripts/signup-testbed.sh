#!/usr/bin/env bash
# Local signup testbed: boots a mock Google OAuth server (which also
# stubs Turnstile siteverify) plus an Oyster server with web signup
# enabled, so the entire /signup flow — Turnstile widget, Google
# consent, one-time admin-key reveal, key dashboard, waitlist review —
# works in a browser with no external accounts and no network.
#
#   ./scripts/signup-testbed.sh              # open mode (default)
#   OYSTER_SIGNUP_MODE=waitlist ./scripts/signup-testbed.sh
#
# Then visit http://localhost:3000/signup. The Turnstile widget uses
# Cloudflare's public always-pass dummy sitekey; the "Google" consent
# screen is the mock's page where you type any email. Waitlist mode:
# approve requests with
#   DATABASE_URL="sqlite:$DB_PATH?mode=rwc" cargo run -p oyster --bin oysterd -- signup list
#
# Storage runs against the local filesystem blob store (no Sui/Walrus/
# Pearl needed — signup doesn't touch the chain). To exercise signup on
# a full on-chain stack instead, run scripts/local-testbed.sh and export
# this script's OYSTER_* / GOOGLE_* / TURNSTILE_* env block before
# starting Oyster there.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK_DIR="${SIGNUP_TESTBED_DIR:-/tmp/oyster-signup-testbed}"
DB_PATH="$WORK_DIR/oyster.db"
MOCK_ADDR="127.0.0.1:9081"
OYSTER_ADDR="127.0.0.1:3000"

mkdir -p "$WORK_DIR"
echo "work dir: $WORK_DIR (db: $DB_PATH)"

cd "$REPO_ROOT"
cargo build -p oyster --bin oysterd --example mock_google

cleanup() {
  echo "shutting down..."
  [[ -n "${MOCK_PID:-}" ]] && kill "$MOCK_PID" 2>/dev/null || true
  [[ -n "${OYSTER_PID:-}" ]] && kill "$OYSTER_PID" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

# --- mock Google (also serves the always-pass siteverify stub) -------------
MOCK_GOOGLE_BIND_ADDR="$MOCK_ADDR" ./target/debug/examples/mock_google &
MOCK_PID=$!

for _ in $(seq 1 20); do
  curl -sf "http://$MOCK_ADDR/certs" >/dev/null && break
  sleep 0.2
done
curl -sf "http://$MOCK_ADDR/certs" >/dev/null || { echo "mock google failed to start"; exit 1; }
echo "mock google up on http://$MOCK_ADDR"

# --- oyster with signup enabled --------------------------------------------
export BIND_ADDR="$OYSTER_ADDR"
export DATABASE_URL="sqlite:$DB_PATH?mode=rwc"
export BLOB_STORE_PATH="$WORK_DIR/blob_store"
export PEARL_SERVICE_SECRET="signup-testbed-unused"

export OYSTER_PUBLIC_BASE_URL="http://localhost:3000"
export GOOGLE_OAUTH_CLIENT_ID="mock-client-id"
export GOOGLE_OAUTH_CLIENT_SECRET="mock-client-secret"
# Cloudflare's public always-pass dummy widget key renders a real widget
# without an account; verification goes to the mock's stub either way.
export TURNSTILE_SITE_KEY="1x00000000000000000000AA"
export TURNSTILE_SECRET_KEY="1x0000000000000000000000000000000AA"
export OYSTER_SIGNUP_MODE="${OYSTER_SIGNUP_MODE:-open}"
export OYSTER_SIGNUP_ALLOWED_DOMAINS="${OYSTER_SIGNUP_ALLOWED_DOMAINS:-}"

# Dev-only endpoint overrides pointing at the mock.
export GOOGLE_OAUTH_AUTH_URL="http://$MOCK_ADDR/auth"
export GOOGLE_OAUTH_TOKEN_URL="http://$MOCK_ADDR/token"
export GOOGLE_OAUTH_JWKS_URL="http://$MOCK_ADDR/certs"
export TURNSTILE_SITEVERIFY_URL="http://$MOCK_ADDR/siteverify"

./target/debug/oysterd serve &
OYSTER_PID=$!

for _ in $(seq 1 30); do
  curl -sf "http://$OYSTER_ADDR/health" >/dev/null && break
  sleep 0.2
done
curl -sf "http://$OYSTER_ADDR/health" >/dev/null || { echo "oyster failed to start"; exit 1; }

echo
echo "ready — signup flow at:  http://localhost:3000/signup"
echo "signup mode:             $OYSTER_SIGNUP_MODE"
echo "review waitlist with:    DATABASE_URL=\"sqlite:$DB_PATH?mode=rwc\" ./target/debug/oysterd signup list"
echo "Ctrl-C to stop."
wait "$OYSTER_PID"
