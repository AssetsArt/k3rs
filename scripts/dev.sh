#!/usr/bin/env bash
set -euo pipefail

# k3rs dev server — auto-restart on code changes using cargo-watch
# Usage: ./scripts/dev.sh [port] [token]

PORT="${1:-6443}"
TOKEN="${2:-demo-token-123}"
DATA_DIR="/tmp/k3rs-data"

echo "🚀 Starting k3rs dev server (port=$PORT, data=$DATA_DIR)"
echo "   Press Ctrl-C to stop"
echo ""

# Ensure cargo-watch is installed
if ! command -v cargo-watch &>/dev/null; then
    echo "📦 Installing cargo-watch..."
    cargo install cargo-watch
fi

# Clean up all ports before starting (server, agent proxy, service proxy, DNS)
./scripts/cleanup-port.sh "$PORT" 2>/dev/null || true

# Watch for changes and restart
cargo watch \
    -x "run --bin k3rs-server -- --port $PORT --token $TOKEN --data-dir $DATA_DIR" \
    -w pkg/ \
    -w cmd/ \
    -i "target/*"
