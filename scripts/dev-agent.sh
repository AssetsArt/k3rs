#!/usr/bin/env bash
set -euo pipefail

# k3rs dev agent — run a local agent that connects to the dev server
# Usage: ./scripts/dev-agent.sh [server] [token] [node-name]

SERVER="${1:-http://127.0.0.1:6443}"
TOKEN="${2:-demo-token-123}"
NODE_NAME="${3:-node-1}"
PROXY_PORT="${4:-6444}"
SERVICE_PROXY_PORT="${5:-10256}"
DNS_PORT="${6:-5353}"

echo "🤖 Starting k3rs agent (node=$NODE_NAME, server=$SERVER)"
echo "   Tunnel proxy:   :$PROXY_PORT"
echo "   Service proxy:  :$SERVICE_PROXY_PORT"
echo "   DNS server:     :$DNS_PORT"
echo "   Press Ctrl-C to stop"
echo ""

# Clean up agent ports
./scripts/cleanup-port.sh "$PROXY_PORT" "$SERVICE_PROXY_PORT" "$DNS_PORT" 2>/dev/null || true

# Ensure cargo-watch is installed
if ! command -v cargo-watch &>/dev/null; then
    echo "📦 Installing cargo-watch..."
    cargo install cargo-watch
fi

# Detect container environment — inotify doesn't work on bind mounts, use polling
WATCH_POLL=""
if [ -f /.dockerenv ] || [ -f /run/.containerenv ]; then
    WATCH_POLL="--poll"
fi

RUST_LOG=debug cargo watch $WATCH_POLL \
    -x "run --bin k3rs-agent -- --server $SERVER --token $TOKEN --node-name $NODE_NAME --proxy-port $PROXY_PORT --service-proxy-port $SERVICE_PROXY_PORT --dns-port $DNS_PORT" \
    -w pkg/ \
    -w cmd/k3rs-agent \
    -i "target/*"
