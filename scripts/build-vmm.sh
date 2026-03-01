#!/bin/bash
# Build k3rs-vmm (Rust) and codesign with virtualization entitlement.
#
# Usage:
#   ./scripts/build-vmm.sh              # Debug build
#   ./scripts/build-vmm.sh release      # Release build
#
# Requirements:
#   - macOS 13+ (Ventura)
#   - Rust toolchain
#   - com.apple.security.virtualization entitlement (for Virtualization.framework)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
VMM_DIR="$PROJECT_ROOT/cmd/k3rs-vmm"

# Default to debug build
BUILD_CONFIG="${1:-debug}"

echo "[build-vmm] Building k3rs-vmm ($BUILD_CONFIG)..."

if [ "$BUILD_CONFIG" = "release" ]; then
    cargo build -p k3rs-vmm --release
else
    cargo build -p k3rs-vmm
fi

BINARY_PATH="$PROJECT_ROOT/target/$BUILD_CONFIG/k3rs-vmm"

if [ ! -f "$BINARY_PATH" ]; then
    echo "[build-vmm] ERROR: Binary not found at $BINARY_PATH"
    exit 1
fi

# Sign with virtualization entitlement (required for Virtualization.framework)
# codesign --entitlements cmd/k3rs-vmm/k3rs-vmm.entitlements --force -s - target/debug/k3rs-vmm
ENTITLEMENTS="$VMM_DIR/k3rs-vmm.entitlements"
if [ -f "$ENTITLEMENTS" ]; then
    codesign --entitlements "$ENTITLEMENTS" --force -s - "$BINARY_PATH"
    echo "[build-vmm] Signed with virtualization entitlement"
else
    echo "[build-vmm] WARNING: entitlements file not found, binary won't be able to boot VMs"
fi

echo "[build-vmm] Built: $BINARY_PATH"
echo "[build-vmm] Size: $(du -h "$BINARY_PATH" | cut -f1)"

# Verify the binary
"$BINARY_PATH" --help 2>/dev/null || true