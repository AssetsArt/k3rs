#!/usr/bin/env bash
set -euo pipefail

# k3rs dev server — auto-restart on code changes using cargo-watch
# Usage: ./scripts/dev.sh [port] [token]

if [ -z "${TMUX:-}" ] && [ "${1:-}" != "--server-only" ]; then
    echo "🚀 Starting k3rs dev environment in tmux..."
    SESSION="k3rs-dev"
    
    # Check if tmux is installed
    if ! command -v tmux &>/dev/null; then
        echo "❌ tmux is not installed. Please install it first (e.g. brew install tmux)."
        exit 1
    fi

    # Kill existing session if it exists to avoid overlapping
    tmux kill-session -t "$SESSION" 2>/dev/null || true
    
    # Create a new session and run the server (pass --server-only so it doesn't loop)
    # Using bash $0 to ensure the script is run directly
    tmux new-session -d -s "$SESSION" "bash \"$0\" --server-only $*"
    
    # Split horizontally for the agent
    # tmux split-window -h "./scripts/dev-agent.sh"
    
    # Split the right pane vertically for the UI
    tmux split-window -h "./scripts/dev-ui.sh"
    
    # Select the first pane (server)
    tmux select-pane -t 0
    
    # Attach to the session
    exec tmux attach-session -t "$SESSION"
fi

if [ "${1:-}" == "--server-only" ]; then
    shift
fi

PORT="${1:-6443}"
TOKEN="${2:-demo-token-123}"
DATA_DIR="/tmp/k3rs-data"
NODE_NAME="master-1"

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
    -x "run --bin k3rs-server -- --port $PORT --token $TOKEN --data-dir $DATA_DIR --node-name $NODE_NAME --allow-colocate" \
    -w pkg/ \
    -w cmd/k3rs-server \
    -i "target/*"
