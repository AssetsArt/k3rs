#!/usr/bin/env bash
set -euo pipefail

# k3rs UI — run the Dioxus management dashboard in dev mode
# Usage: ./scripts/dev-ui.sh
# Requires: dx CLI (cargo install dioxus-cli)

echo "🎨 Starting k3rs Management UI (Dioxus)"
echo "   Press Ctrl-C to stop"
echo ""

# Ensure dx CLI is installed
if ! command -v dx &>/dev/null; then
    echo "📦 Installing dioxus-cli..."
    cargo install dioxus-cli
fi

cd cmd/k3rs-ui
dx serve
