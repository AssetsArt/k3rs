#!/bin/bash
# cleanup-network.sh — Remove all k3rs network artifacts from macOS.
# Run with: sudo ./scripts/cleanup-network.sh
#
# This cleans up:
#   1. All k3rs processes (VMs, agent, server, vpc, dev, ui)
#   2. pfctl NAT rules (restores /etc/pf.conf defaults)
#   3. IP forwarding (disable)
#   4. Routes via utun (pod CIDR)
#   5. utun interfaces (auto-removed when owning process dies)

set -e

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m'

log()  { echo -e "${GREEN}[cleanup]${NC} $*"; }
warn() { echo -e "${YELLOW}[cleanup]${NC} $*"; }

# ─── 1. Kill all k3rs processes ──────────────────────────────────

log "Stopping k3rs processes..."
for proc in k3rs-vmm k3rs-agent k3rs-server k3rs-vpc k3rs-dev k3rs-ui k3rsctl; do
    pids=$(pgrep -x "$proc" 2>/dev/null || true)
    if [ -n "$pids" ]; then
        kill $pids 2>/dev/null || true
        log "  killed $proc (pid: $(echo $pids | tr '\n' ' '))"
    fi
done

# Also kill any k3rs-ui variants (k3rs-ui-<hash>)
pids=$(pgrep -f "k3rs-ui-" 2>/dev/null || true)
if [ -n "$pids" ]; then
    kill $pids 2>/dev/null || true
    log "  killed k3rs-ui variants"
fi

sleep 2

# Force-kill any remaining
remaining=$(pgrep -f "k3rs-" 2>/dev/null || true)
if [ -n "$remaining" ]; then
    warn "Force-killing remaining k3rs processes..."
    kill -9 $remaining 2>/dev/null || true
    sleep 1
fi

# ─── 2. Restore pfctl rules ─────────────────────────────────────

log "Restoring pfctl rules..."

# Flush the k3rs anchor (if it exists)
pfctl -a com.k3rs.nat -F all 2>/dev/null || true

# Reload default /etc/pf.conf (removes any k3rs-nat rules from main ruleset)
if [ -f /etc/pf.conf ]; then
    # Remove k3rs-nat lines if they were injected
    if grep -q "k3rs-nat" /etc/pf.conf 2>/dev/null; then
        warn "  /etc/pf.conf contains k3rs-nat lines (should not happen, rules are loaded transiently)"
    fi
    pfctl -f /etc/pf.conf 2>/dev/null || true
    log "  pfctl rules restored from /etc/pf.conf"
else
    # No pf.conf — just flush everything
    pfctl -F all 2>/dev/null || true
    log "  pfctl rules flushed"
fi

# ─── 3. Disable IP forwarding ───────────────────────────────────

current=$(sysctl -n net.inet.ip.forwarding 2>/dev/null)
if [ "$current" = "1" ]; then
    sysctl -w net.inet.ip.forwarding=0 >/dev/null 2>&1
    log "IP forwarding disabled"
else
    log "IP forwarding already disabled"
fi

# ─── 4. Remove routes ───────────────────────────────────────────

log "Removing k3rs routes..."
POD_CIDR="10.42.0.0/16"

if netstat -rn | grep -q "10.42"; then
    route delete -net "$POD_CIDR" 2>/dev/null || true
    log "  removed route for $POD_CIDR"
else
    log "  no k3rs routes found"
fi

# Remove utun peer route if present
if netstat -rn | grep -q "10.254.254"; then
    route delete -host 10.254.254.2 2>/dev/null || true
    log "  removed utun peer route"
fi

# ─── 5. utun cleanup ────────────────────────────────────────────

# utun devices are automatically destroyed when the owning process exits.
# Since we killed k3rs-agent above, the utun should be gone already.
if ifconfig | grep -q "10.254.254"; then
    warn "utun with 10.254.254.x still exists (will be cleaned when fd closes)"
else
    log "utun interfaces clean"
fi

# ─── 6. Clean up runtime sockets ────────────────────────────────

log "Cleaning up runtime sockets..."
rm -f /tmp/k3rs-data/runtime/vms/vmm-*.sock 2>/dev/null || true
rm -f /tmp/k3rs-data/k3rs-vpc.sock 2>/dev/null || true

# ─── Done ────────────────────────────────────────────────────────

echo ""
log "✓ Network cleanup complete!"
echo ""
echo "  Verify with:"
echo "    sysctl net.inet.ip.forwarding     # should be 0"
echo "    netstat -rn | grep 10.42          # should be empty"
echo "    pgrep k3rs                        # should be empty"
echo "    ifconfig | grep 10.254            # should be empty"
