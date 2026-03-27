#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Source .env if it exists
if [[ -f "$SCRIPT_DIR/.env" ]]; then
  set -a
  source "$SCRIPT_DIR/.env"
  set +a
fi

# Default KB data directory
export KB_DIR="${KB_DIR:-$SCRIPT_DIR/data}"

# Resolve binary: container path first, then local build
BINARY="/usr/local/lib/mcp-servers/knowledge-base"
[[ -x "$BINARY" ]] || BINARY="$SCRIPT_DIR/server/target/release/knowledge-base"
exec "$BINARY"
