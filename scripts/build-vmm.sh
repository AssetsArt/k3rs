#!/bin/bash
# Build k3rs-vmm (Swift) and copy the binary to the Cargo target directory.
#
# Usage:
#   ./scripts/build-vmm.sh              # Debug build
#   ./scripts/build-vmm.sh release      # Release build
#
# Requirements:
#   - macOS 13+ (Ventura)
#   - Xcode 15+ with Swift 5.9+
#   - com.apple.security.virtualization entitlement (for production use)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
VMM_DIR="$PROJECT_ROOT/cmd/k3rs-vmm"

# Default to debug build
BUILD_CONFIG="${1:-debug}"
SWIFT_CONFIG="debug"
if [ "$BUILD_CONFIG" = "release" ]; then
    SWIFT_CONFIG="release"
fi

echo "[build-vmm] Building k3rs-vmm ($SWIFT_CONFIG)..."

cd "$VMM_DIR"

# Resolve Swift dependencies
swift package resolve

# Build
swift build -c "$SWIFT_CONFIG"

# Find the built binary
BINARY_PATH="$VMM_DIR/.build/$SWIFT_CONFIG/k3rs-vmm"

if [ ! -f "$BINARY_PATH" ]; then
    echo "[build-vmm] ERROR: Binary not found at $BINARY_PATH"
    exit 1
fi

# Copy to Cargo target directory for k3rs-agent to find
TARGET_DIR="$PROJECT_ROOT/target/$BUILD_CONFIG"
mkdir -p "$TARGET_DIR"
cp "$BINARY_PATH" "$TARGET_DIR/k3rs-vmm"

# Sign with virtualization entitlement (required for Virtualization.framework)
ENTITLEMENTS="$VMM_DIR/k3rs-vmm.entitlements"
if [ -f "$ENTITLEMENTS" ]; then
    codesign --entitlements "$ENTITLEMENTS" --force -s - "$TARGET_DIR/k3rs-vmm"
    echo "[build-vmm] Signed with virtualization entitlement"
else
    echo "[build-vmm] WARNING: entitlements file not found, binary won't be able to boot VMs"
fi

echo "[build-vmm] Built: $TARGET_DIR/k3rs-vmm"
echo "[build-vmm] Size: $(du -h "$TARGET_DIR/k3rs-vmm" | cut -f1)"

# Verify the binary
"$TARGET_DIR/k3rs-vmm" --help 2>/dev/null || true