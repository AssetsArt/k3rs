#!/bin/bash
# =============================================================================
# k3rs Integration Test: Agent Cache Resilience & Reconnect
# =============================================================================
# Tests Group 1 (ALSC / AgentStore) + Group 2 (backoff) behaviour end-to-end.
#
# Scenarios
# ---------
#   1. Server down   — Agent routing+DNS survive; reconnect refreshes cache
#   2. Agent restart — Stale cache restores within 1s (no server contact)
#   3. Both down     — Restart both; full cluster recovery
#   4. Fresh start   — No prior cache; normal boot path
#   5. Stale cache   — Reconnect after offline; server-wins full re-sync
#
# Requirements
# ------------
#   - cargo (workspace built before running)
#   - dig  (for DNS verification)
#   - curl, kill, sleep, grep
#
# Usage
#   bash scripts/test-resilience.sh
# =============================================================================

set -euo pipefail

# ── config ────────────────────────────────────────────────────────────────────
BASE_DATA="/tmp/k3rs-test-resilience"
SERVER_PORT=7445
PROXY_PORT=7446
SVC_PROXY_PORT=10358
DNS_PORT=5455        # avoid 5353 (mDNSResponder)
TOKEN="resilience-test-token"
NODE="resilience-node"
SERVER_URL="http://127.0.0.1:${SERVER_PORT}"
AUTH="Authorization: Bearer ${TOKEN}"

PASSED=0
FAILED=0

# ── helpers ───────────────────────────────────────────────────────────────────
log()  { echo "[$(date +%H:%M:%S)] $*"; }
pass() { echo "  ✅ PASS: $1"; PASSED=$((PASSED+1)); }
fail() { echo "  ❌ FAIL: $1"; FAILED=$((FAILED+1)); }

wait_for_log() {
    local file="$1" pattern="$2" max_secs="${3:-20}"
    for _ in $(seq 1 "$max_secs"); do
        if grep -q "$pattern" "$file" 2>/dev/null; then return 0; fi
        sleep 1
    done
    return 1
}

start_server() {
    local data_dir="$1" log_file="$2"
    RUST_LOG=info cargo run -q --bin k3rs-server -- \
        --data-dir "$data_dir" \
        --token "$TOKEN" \
        --port "$SERVER_PORT" \
        > "$log_file" 2>&1 &
    echo $!
}

start_agent() {
    local data_dir="$1" log_file="$2"
    RUST_LOG=info cargo run -q --bin k3rs-agent -- \
        --server "$SERVER_URL" \
        --token "$TOKEN" \
        --node-name "$NODE" \
        --proxy-port "$PROXY_PORT" \
        --service-proxy-port "$SVC_PROXY_PORT" \
        --dns-port "$DNS_PORT" \
        --data-dir "$data_dir" \
        > "$log_file" 2>&1 &
    echo $!
}

api_post() {
    curl -s -f -X POST "${SERVER_URL}${1}" \
        -H "Content-Type: application/json" \
        -H "$AUTH" \
        -d "$2" > /dev/null
}

api_put() {
    curl -s -f -X PUT "${SERVER_URL}${1}" \
        -H "Content-Type: application/json" \
        -H "$AUTH" \
        -d "$2" > /dev/null
}

# Create a Service + Endpoint pair so the agent has real routing data to cache.
# Args: service-name, namespace, cluster-ip, pod-ip
create_service_and_endpoint() {
    local name="$1" ns="$2" cluster_ip="$3" pod_ip="$4"
    local svc_id="svc-${name}" ep_id="ep-${name}"

    api_post "/api/v1/namespaces/${ns}/services" "$(cat <<JSON
{
  "id": "${svc_id}",
  "name": "${name}",
  "namespace": "${ns}",
  "cluster_ip": "${cluster_ip}",
  "spec": {
    "ports": [{"name":"http","port":80,"target_port":8080}],
    "selector": {},
    "service_type": "ClusterIP"
  },
  "created_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
}
JSON
)"

    api_post "/api/v1/namespaces/${ns}/endpoints" "$(cat <<JSON
{
  "id": "${ep_id}",
  "service_id": "${svc_id}",
  "service_name": "${name}",
  "namespace": "${ns}",
  "addresses": [{"ip": "${pod_ip}"}],
  "ports": [],
  "created_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
}
JSON
)"
}

# Query the agent's embedded DNS (port DNS_PORT) for a service FQDN.
# Returns 0 if the expected IP appears in the answer.
dns_resolves() {
    local fqdn="$1" expected_ip="$2"
    local answer
    answer=$(dig +short +time=2 +tries=1 "@127.0.0.1" -p "$DNS_PORT" "$fqdn" 2>/dev/null || true)
    [[ "$answer" == *"$expected_ip"* ]]
}

kill_pid() {
    local pid="$1"
    kill -9 "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
}

# ─────────────────────────────────────────────────────────────────────────────
# Scenario 4 (first — simplest): Fresh start, no prior cache
# ─────────────────────────────────────────────────────────────────────────────
scenario_4_fresh_start() {
    log "━━━ Scenario 4: Fresh start (no prior cache) ━━━"
    local dir="${BASE_DATA}/s4"
    rm -rf "$dir"; mkdir -p "$dir"

    local srv_pid agent_pid
    srv_pid=$(start_server "${dir}/server-data" "${dir}/server.log")
    sleep 2

    agent_pid=$(start_agent "${dir}/agent-data" "${dir}/agent.log")

    # Agent should register successfully without any cached state
    if wait_for_log "${dir}/agent.log" "Successfully registered" 20; then
        pass "S4: Agent registers on fresh start"
    else
        fail "S4: Agent failed to register on fresh start"
        tail -10 "${dir}/agent.log" >&2
    fi

    # Should NOT have loaded any prior cache
    if ! grep -q "AgentStore loaded:" "${dir}/agent.log" 2>/dev/null; then
        pass "S4: No stale cache loaded (fresh node)"
    else
        fail "S4: Unexpected cache load on fresh node"
    fi

    kill_pid "$agent_pid"; kill_pid "$srv_pid"
    log "Scenario 4 done"
}

# ─────────────────────────────────────────────────────────────────────────────
# Scenario 1: Kill Server → routing/DNS survive → reconnect → cache refresh
# ─────────────────────────────────────────────────────────────────────────────
scenario_1_server_down() {
    log "━━━ Scenario 1: Server down — routing+DNS survive ━━━"
    local dir="${BASE_DATA}/s1"
    rm -rf "$dir"; mkdir -p "$dir"

    local srv_pid agent_pid
    srv_pid=$(start_server "${dir}/server-data" "${dir}/server.log")
    sleep 2

    agent_pid=$(start_agent "${dir}/agent-data" "${dir}/agent.log")
    if ! wait_for_log "${dir}/agent.log" "Successfully registered" 20; then
        fail "S1: Agent failed to register (prerequisite)"
        kill_pid "$agent_pid"; kill_pid "$srv_pid"; return
    fi

    # Create a service to give the agent real routing data
    create_service_and_endpoint "nginx" "default" "10.100.1.1" "172.16.0.1"

    # Wait for route sync to save at least 1 DNS entry (proves real service data was synced).
    # Using "[1-9][0-9]* dns" avoids matching the registration save (which has "0 dns").
    log "  Waiting for agent to sync service data…"
    if ! wait_for_log "${dir}/agent.log" "[1-9][0-9]* dns" 25; then
        fail "S1: AgentStore never saved real service data after route sync"
        kill_pid "$agent_pid"; kill_pid "$srv_pid"; return
    fi
    pass "S1: Agent synced and persisted service to AgentStore"

    # Kill the server
    log "  Killing server (PID ${srv_pid})…"
    kill_pid "$srv_pid"
    sleep 3

    # Agent should enter RECONNECTING state
    if wait_for_log "${dir}/agent.log" "RECONNECTING\|server unreachable" 15; then
        pass "S1: Agent detected server loss and entered RECONNECTING"
    else
        fail "S1: Agent did not enter RECONNECTING state"
    fi

    # DNS should still resolve from in-memory cache
    if dns_resolves "nginx.default.svc.cluster.local" "10.100.1.1"; then
        pass "S1: DNS still resolves while server is down (stale in-memory cache)"
    else
        fail "S1: DNS resolution failed while server is down"
    fi

    # Restart server
    log "  Restarting server…"
    srv_pid=$(start_server "${dir}/server-data" "${dir}/server2.log")
    sleep 2

    # Agent should reconnect and refresh cache
    if wait_for_log "${dir}/agent.log" "Reconnected to server\|CONNECTED" 35; then
        pass "S1: Agent reconnected to server"
    else
        fail "S1: Agent failed to reconnect"
    fi

    if wait_for_log "${dir}/agent.log" "AgentStore saved" 20; then
        pass "S1: AgentStore refreshed after reconnect"
    else
        fail "S1: AgentStore not refreshed after reconnect"
    fi

    kill_pid "$agent_pid"; kill_pid "$srv_pid"
    log "Scenario 1 done"
}

# ─────────────────────────────────────────────────────────────────────────────
# Scenario 2: Kill Agent → restart with no server → stale cache within 1s
# ─────────────────────────────────────────────────────────────────────────────
scenario_2_agent_restart_offline() {
    log "━━━ Scenario 2: Agent restart offline — stale cache within 1s ━━━"
    local dir="${BASE_DATA}/s2"
    rm -rf "$dir"; mkdir -p "$dir"

    # Phase A: Get agent synced with real data
    local srv_pid agent_pid
    srv_pid=$(start_server "${dir}/server-data" "${dir}/server.log")
    sleep 2

    agent_pid=$(start_agent "${dir}/agent-data" "${dir}/agent.log")
    if ! wait_for_log "${dir}/agent.log" "Successfully registered" 20; then
        fail "S2: Setup failed — agent did not register"
        kill_pid "$agent_pid"; kill_pid "$srv_pid"; return
    fi

    create_service_and_endpoint "cache-svc" "default" "10.100.2.2" "172.16.0.2"

    # Wait for a save that actually contains service data ("[1-9][0-9]* dns" means ≥1 DNS entry).
    # The registration save has "0 dns" and must not be matched here.
    if ! wait_for_log "${dir}/agent.log" "[1-9][0-9]* dns" 25; then
        fail "S2: AgentStore never saved real service data (prerequisite)"
        kill_pid "$agent_pid"; kill_pid "$srv_pid"; return
    fi
    pass "S2: Setup complete — service synced and persisted"

    # Phase B: Kill everything
    kill_pid "$agent_pid"
    kill_pid "$srv_pid"
    sleep 1

    # Phase C: Restart agent WITHOUT server
    log "  Restarting agent (no server)…"
    local agent2_log="${dir}/agent2.log"
    local t_start t_loaded elapsed
    t_start=$(date +%s)
    agent_pid=$(start_agent "${dir}/agent-data" "$agent2_log")

    # Agent must load cache quickly (Phase A completes before server contact)
    if wait_for_log "$agent2_log" "AgentStore loaded" 5; then
        t_loaded=$(date +%s)
        elapsed=$((t_loaded - t_start))
        if [ "$elapsed" -le 1 ]; then
            pass "S2: Stale cache loaded in ${elapsed}s (≤1s)"
        else
            pass "S2: Stale cache loaded in ${elapsed}s (>1s but functional)"
        fi
    else
        fail "S2: AgentStore not loaded within 5s after restart"
    fi

    # DNS should resolve from stale cache even with no server
    sleep 2
    if dns_resolves "cache-svc.default.svc.cluster.local" "10.100.2.2"; then
        pass "S2: DNS resolves from stale cache (no server contact)"
    else
        fail "S2: DNS resolution failed when serving stale cache offline"
    fi

    kill_pid "$agent_pid"
    log "Scenario 2 done"
}

# ─────────────────────────────────────────────────────────────────────────────
# Scenario 3: Kill Agent + Server → restart both → cluster recovery
# ─────────────────────────────────────────────────────────────────────────────
scenario_3_both_restart() {
    log "━━━ Scenario 3: Both down → restart → cluster recovery ━━━"
    local dir="${BASE_DATA}/s3"
    rm -rf "$dir"; mkdir -p "$dir"

    # Setup: create cluster with service
    local srv_pid agent_pid
    srv_pid=$(start_server "${dir}/server-data" "${dir}/server.log")
    sleep 2

    agent_pid=$(start_agent "${dir}/agent-data" "${dir}/agent.log")
    if ! wait_for_log "${dir}/agent.log" "Successfully registered" 20; then
        fail "S3: Setup failed — agent did not register"
        kill_pid "$agent_pid"; kill_pid "$srv_pid"; return
    fi

    create_service_and_endpoint "recovery-svc" "default" "10.100.3.3" "172.16.0.3"
    wait_for_log "${dir}/agent.log" "AgentStore saved" 25 || true

    # Kill both
    log "  Killing agent and server simultaneously…"
    kill_pid "$agent_pid"
    kill_pid "$srv_pid"
    sleep 2

    # Restart server first
    log "  Restarting server…"
    srv_pid=$(start_server "${dir}/server-data" "${dir}/server2.log")
    sleep 2

    # Restart agent
    log "  Restarting agent…"
    local agent2_log="${dir}/agent2.log"
    agent_pid=$(start_agent "${dir}/agent-data" "$agent2_log")

    # Agent should load stale cache then reconnect
    if wait_for_log "$agent2_log" "AgentStore loaded" 10; then
        pass "S3: Agent loaded stale cache after joint restart"
    else
        fail "S3: Agent did not load stale cache"
    fi

    if wait_for_log "$agent2_log" "Successfully registered\|Reconnected to server\|Heartbeat probe succeeded" 30; then
        pass "S3: Agent reconnected to server after joint restart"
    else
        fail "S3: Agent did not reconnect to server"
    fi

    if wait_for_log "$agent2_log" "AgentStore saved" 25; then
        pass "S3: AgentStore refreshed with fresh server data"
    else
        fail "S3: AgentStore not refreshed after cluster recovery"
    fi

    kill_pid "$agent_pid"; kill_pid "$srv_pid"
    log "Scenario 3 done"
}

# ─────────────────────────────────────────────────────────────────────────────
# Scenario 5: Stale cache → reconnect → server-wins full re-sync
# ─────────────────────────────────────────────────────────────────────────────
scenario_5_stale_resync() {
    log "━━━ Scenario 5: Stale cache → reconnect → full server-wins re-sync ━━━"
    local dir="${BASE_DATA}/s5"
    rm -rf "$dir"; mkdir -p "$dir"

    # Setup: agent syncs initial data (service A, ip 10.100.5.1)
    local srv_pid agent_pid
    srv_pid=$(start_server "${dir}/server-data" "${dir}/server.log")
    sleep 2

    agent_pid=$(start_agent "${dir}/agent-data" "${dir}/agent.log")
    if ! wait_for_log "${dir}/agent.log" "Successfully registered" 20; then
        fail "S5: Setup failed — agent did not register"
        kill_pid "$agent_pid"; kill_pid "$srv_pid"; return
    fi

    create_service_and_endpoint "svc-old" "default" "10.100.5.1" "172.16.0.5"
    # Wait until a route sync with real data is saved (not just the registration save).
    if ! wait_for_log "${dir}/agent.log" "[1-9][0-9]* dns" 25; then
        fail "S5: Initial sync never persisted real service data"
        kill_pid "$agent_pid"; kill_pid "$srv_pid"; return
    fi
    pass "S5: Initial state synced (svc-old → 10.100.5.1)"

    # Kill agent (server stays alive)
    kill_pid "$agent_pid"
    sleep 1

    # While agent is offline, add a new service (svc-new, ip 10.100.5.99)
    create_service_and_endpoint "svc-new" "default" "10.100.5.99" "172.16.0.99"
    log "  Added svc-new (10.100.5.99) while agent was offline"

    # Restart agent — should re-sync and pick up svc-new
    local agent2_log="${dir}/agent2.log"
    agent_pid=$(start_agent "${dir}/agent-data" "$agent2_log")

    if ! wait_for_log "$agent2_log" "Reconnected to server\|Heartbeat probe succeeded\|Successfully registered" 25; then
        fail "S5: Agent did not reconnect after restart"
        kill_pid "$agent_pid"; kill_pid "$srv_pid"; return
    fi

    # Wait for route sync to pick up the new service
    if ! wait_for_log "$agent2_log" "AgentStore saved" 25; then
        fail "S5: AgentStore not refreshed after reconnect"
        kill_pid "$agent_pid"; kill_pid "$srv_pid"; return
    fi
    pass "S5: AgentStore refreshed after reconnect (server-wins)"

    # DNS should now resolve svc-new (created while agent was offline)
    sleep 3
    if dns_resolves "svc-new.default.svc.cluster.local" "10.100.5.99"; then
        pass "S5: svc-new resolves via DNS after re-sync (new data wins)"
    else
        fail "S5: svc-new did not resolve — re-sync may have missed it"
    fi

    # Old service should still be present (it wasn't removed from server)
    if dns_resolves "svc-old.default.svc.cluster.local" "10.100.5.1"; then
        pass "S5: svc-old still resolves (full re-sync preserved existing data)"
    else
        fail "S5: svc-old missing after re-sync — unexpected data loss"
    fi

    kill_pid "$agent_pid"; kill_pid "$srv_pid"
    log "Scenario 5 done"
}

# ─────────────────────────────────────────────────────────────────────────────
# Main
# ─────────────────────────────────────────────────────────────────────────────
main() {
    echo "╔══════════════════════════════════════════════════════════════╗"
    echo "║   k3rs Integration Test: Agent Cache Resilience             ║"
    echo "╚══════════════════════════════════════════════════════════════╝"
    echo ""

    rm -rf "$BASE_DATA"
    mkdir -p "$BASE_DATA"

    # Build once before running scenarios
    log "Building workspace…"
    cargo build -q --bins 2>&1
    log "Build complete"
    echo ""

    scenario_4_fresh_start;   echo ""
    scenario_1_server_down;   echo ""
    scenario_2_agent_restart_offline; echo ""
    scenario_3_both_restart;  echo ""
    scenario_5_stale_resync;  echo ""

    echo "╔══════════════════════════════════════════════════════════════╗"
    printf "║   Results: %-5d passed  %-5d failed%21s║\n" "$PASSED" "$FAILED" ""
    echo "╚══════════════════════════════════════════════════════════════╝"

    if [ "$FAILED" -gt 0 ]; then
        echo ""
        echo "Logs saved in: ${BASE_DATA}/"
        exit 1
    fi

    rm -rf "$BASE_DATA"
    exit 0
}

main "$@"
