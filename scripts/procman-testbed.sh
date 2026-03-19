#!/usr/bin/env bash
set -euo pipefail

# ---------------------------------------------------------------------------
# Oyster + Pearl local testbed — starts the full stack via procman.
# ---------------------------------------------------------------------------

WALRUS_WORKING_DIR="${WALRUS_WORKING_DIR:-$HOME/src/walrus/working_dir}"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

usage() {
  cat <<EOF
Usage: $(basename "$0") [OPTIONS]

Start the full Sui + Walrus + Pearl + Oyster stack via procman.

Options:
  --walrus-working-dir <path>  Walrus working directory (default: ~/src/walrus/working_dir)
  --stop                       Kill existing testbed and exit
  --help                       Show this help message

Prerequisites:
  - cargo, sui, walrus, grpcurl, jq, curl, aws, procman in PATH
EOF
}

die() { echo "error: $*" >&2; exit 1; }

check_prereqs() {
  local missing=()
  for cmd in cargo sui walrus grpcurl jq curl aws procman; do
    if ! command -v "$cmd" &>/dev/null; then
      missing+=("$cmd")
    fi
  done
  if (( ${#missing[@]} )); then
    die "missing required tools: ${missing[*]}. Install them before running this script."
  fi
}

# --- Parse args ---
while (( $# )); do
  case "$1" in
    --walrus-working-dir)
      WALRUS_WORKING_DIR="$2"; shift 2 ;;
    --stop)
      (cd "$REPO_ROOT" && procman stop) 2>/dev/null || true
      echo "Done."
      exit 0 ;;
    --help|-h)
      usage; exit 0 ;;
    *)
      die "unknown option: $1 (try --help)" ;;
  esac
done

check_prereqs

# Kill stale testbed.
(cd "$REPO_ROOT" && procman stop) 2>/dev/null || true

# Clean walrus working dir + stale DBs.
if [[ -d "$WALRUS_WORKING_DIR" ]]; then
  echo "Cleaning existing Walrus working directory at $WALRUS_WORKING_DIR..."
  rm -rf "$WALRUS_WORKING_DIR"
fi
rm -f "$REPO_ROOT"/oyster.db*

# Build.
echo "Building pearl and oyster..."
(cd "$REPO_ROOT" && cargo build -p pearl -p oyster)

# Start the full stack.
export WALRUS_WORKING_DIR
cd "$REPO_ROOT" && exec procman serve
