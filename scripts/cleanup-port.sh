#!/usr/bin/env bash
set -euo pipefail

# Kill any process listening on the given port(s)
# Usage: ./scripts/cleanup-port.sh [port1] [port2] ...
# Default: cleans up 6443 (server) and 6444 (agent proxy)

PORTS=("${@:-6443 6444}")

for PORT in ${PORTS[@]}; do
    PIDS=$(lsof -ti :"$PORT" 2>/dev/null || true)
    if [ -n "$PIDS" ]; then
        echo "🧹 Killing process(es) on port $PORT: $PIDS"
        echo "$PIDS" | xargs kill -9 2>/dev/null || true
    else
        echo "✅ Port $PORT is free"
    fi
done
