#!/usr/bin/env bash
set -euo pipefail

# Setup Podman machine with KVM support for Firecracker testing.
#
# On macOS Apple Silicon, this recreates the Podman machine to attempt
# nested virtualization (KVM inside the VM). Requires macOS 15+ and
# Podman 5.3+ for Apple HV nested virt support.
#
# On native Linux, KVM should already be available (/dev/kvm).
#
# Usage:
#   ./scripts/setup-podman-kvm.sh

RED='\033[0;31m'
GREEN='\033[0;32m'
CYAN='\033[0;36m'
YELLOW='\033[1;33m'
NC='\033[0m'

MACHINE_NAME="podman-machine-default"

echo "╔══════════════════════════════════════════════════════════╗"
echo "║  k3rs Podman Machine KVM Setup                          ║"
echo "╚══════════════════════════════════════════════════════════╝"
echo

# ─── Platform check ─────────────────────────────────────────────
OS="$(uname -s)"

if [ "$OS" = "Linux" ]; then
    if [ -e /dev/kvm ]; then
        echo -e "${GREEN}KVM is already available on this Linux host.${NC}"
        echo "No Podman machine setup needed — use ./scripts/dev-podman.sh --kvm"
    else
        echo -e "${RED}KVM not available on this Linux host.${NC}"
        echo "Check that your CPU supports virtualization and KVM modules are loaded:"
        echo "  sudo modprobe kvm"
        echo "  sudo modprobe kvm_intel  # or kvm_amd"
        echo "  ls -la /dev/kvm"
    fi
    exit 0
fi

if [ "$OS" != "Darwin" ]; then
    echo -e "${RED}Unsupported platform: $OS${NC}"
    exit 1
fi

# ─── macOS — check Podman ───────────────────────────────────────
if ! command -v podman &>/dev/null; then
    echo -e "${RED}Podman not found. Install with: brew install podman${NC}"
    exit 1
fi

PODMAN_VERSION=$(podman --version | awk '{print $3}')
echo -e "Podman version: ${CYAN}$PODMAN_VERSION${NC}"
echo -e "macOS kernel:   ${CYAN}$(uname -r)${NC}"
echo -e "Architecture:   ${CYAN}$(uname -m)${NC}"
echo

# ─── Check existing machine ────────────────────────────────────
if podman machine inspect "$MACHINE_NAME" &>/dev/null; then
    # Check if KVM already works
    if podman machine ssh -- test -e /dev/kvm 2>/dev/null; then
        echo -e "${GREEN}KVM is already available inside Podman machine!${NC}"
        echo "Use: ./scripts/dev-podman.sh --all --kvm"
        exit 0
    fi

    echo -e "${YELLOW}Existing Podman machine does not have KVM.${NC}"
    echo -e "To recreate with nested virtualization, the current machine must be removed."
    echo
    echo -e "${YELLOW}WARNING: This will delete the existing Podman machine and all its data.${NC}"
    echo -e "Volumes (cargo cache, k3rs-data) are preserved as podman volumes."
    echo
    read -rp "Proceed? [y/N] " confirm
    if [[ ! "$confirm" =~ ^[Yy]$ ]]; then
        echo "Aborted."
        exit 0
    fi

    echo
    echo -e "${CYAN}Stopping and removing existing machine...${NC}"
    podman machine stop "$MACHINE_NAME" 2>/dev/null || true
    podman machine rm -f "$MACHINE_NAME" 2>/dev/null || true
fi

# ─── Create new machine ────────────────────────────────────────
echo
echo -e "${CYAN}Creating Podman machine with optimized settings...${NC}"
echo "  CPUs:    10"
echo "  Memory:  12 GB"
echo "  Disk:    100 GB"
echo "  Rootful: yes (needed for --privileged containers)"
echo

podman machine init \
    --cpus 10 \
    --memory 12000 \
    --disk-size 100 \
    --rootful \
    "$MACHINE_NAME"

echo
echo -e "${CYAN}Starting machine...${NC}"
podman machine start "$MACHINE_NAME"

# ─── Verify KVM ─────────────────────────────────────────────────
echo
echo -e "${CYAN}Checking for KVM inside the VM...${NC}"
sleep 2

if podman machine ssh -- test -e /dev/kvm 2>/dev/null; then
    echo -e "${GREEN}KVM is available! Firecracker can run inside Podman.${NC}"
    echo
    echo "Next steps:"
    echo "  ./scripts/dev-podman.sh --all --kvm"
else
    echo -e "${YELLOW}KVM is NOT available inside the Podman machine.${NC}"
    echo
    echo "This is expected on Apple Silicon — Apple's Virtualization.framework"
    echo "supports nested virtualization in macOS 15+ (Sequoia), but Podman's"
    echo "Apple HV provider may not expose it yet."
    echo
    echo "Options:"
    echo "  1. Test Firecracker on native Linux (recommended)"
    echo "  2. Use a Linux VM with QEMU + KVM (e.g., UTM with nested virt)"
    echo "  3. Wait for Podman to add --nested-virt support for Apple HV"
    echo
    echo "The k3rs-agent will auto-detect and fall back to OCI runtime (crun/youki)"
    echo "when KVM is not available."
fi
