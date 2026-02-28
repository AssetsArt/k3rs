#!/bin/bash
set -euo pipefail

echo "=========================================================="
echo " k3rs Integration Test: Agent Crash & OCI Recovery"
echo "=========================================================="

DATA_DIR="/tmp/k3rs-test-recovery"
rm -rf "$DATA_DIR"
mkdir -p "$DATA_DIR"

SERVER_LOG="$DATA_DIR/server.log"
AGENT_LOG="$DATA_DIR/agent.log"

echo "[1] Starting k3rs-server (control plane)..."
RUST_LOG=info cargo run --bin k3rs-server -- \
  --data-dir "$DATA_DIR/server-data" \
  --token "test-recovery-token" \
  --port 6445 \
  > "$SERVER_LOG" 2>&1 &
SERVER_PID=$!

echo "[1] Wait for server to be ready..."
sleep 2

echo "[2] Starting k3rs-agent (data plane)..."
RUST_LOG=info cargo run --bin k3rs-agent -- \
  --server "http://127.0.0.1:6445" \
  --token "test-recovery-token" \
  --node-name "test-node" \
  --proxy-port 6446 \
  --service-proxy-port 10257 \
  --dns-port 5354 \
  > "$AGENT_LOG" 2>&1 &
AGENT_PID=$!

echo "[2] Wait for agent to register..."
sleep 10

echo "[3] Deploying a test pod..."
POD_JSON=$(cat <<EOF
{
  "id": "rec-pod-1",
  "name": "nginx-recovery",
  "namespace": "default",
  "spec": {
    "containers": [
      {
        "name": "sleep",
        "image": "docker.io/library/busybox:latest",
        "command": ["sleep"],
        "args": ["3600"]
      }
    ]
  },
  "status": "Scheduled",
  "status_message": null,
  "node_name": "test-node",
  "labels": {},
  "restart_count": 0,
  "created_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
}
EOF
)

echo "Creating pod..."
curl -s -X POST "http://127.0.0.1:6445/api/v1/namespaces/default/pods" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer test-recovery-token" \
  -d "$POD_JSON" || true

echo "[4] Wait for pod to enter Running state..."
for i in {1..20}; do
  STATUS=$(curl -s -H "Authorization: Bearer test-recovery-token" "http://127.0.0.1:6445/api/v1/namespaces/default/pods/nginx-recovery" | grep -o '"status":"[^"]*"' | cut -d'"' -f4 || echo "")
  if [ "$STATUS" == "Running" ]; then
    echo "    Pod is Running!"
    break
  fi
  sleep 2
done

if [ "$STATUS" != "Running" ]; then
  echo "ERROR: Pod failed to start. Status: $STATUS"
  kill -9 $SERVER_PID $AGENT_PID || true
  exit 1
fi

echo "[5] Verifying container is running via youki/crun state..."
POD_INFO=$(curl -s -H "Authorization: Bearer test-recovery-token" "http://127.0.0.1:6445/api/v1/namespaces/default/pods/nginx-recovery")
ACTUAL_POD_ID=$(echo "$POD_INFO" | grep -o '"id":"[^"]*"' | cut -d'"' -f4)

RUNTIME_BIN=$(grep "Detected OCI runtime" "$AGENT_LOG" | head -n1 | awk '{print $8}')
if [ -z "$RUNTIME_BIN" ]; then
    RUNTIME_BIN="crun" # fallback guess
fi
echo "    Using runtime: $RUNTIME_BIN"
echo "    Actual Pod ID: $ACTUAL_POD_ID"

RUNTIME_ROOT="/tmp/k3rs-runtime/state"
CLI_STATE=$($RUNTIME_BIN --root "$RUNTIME_ROOT" state "$ACTUAL_POD_ID" || echo "failed")
if [[ "$CLI_STATE" == *"running"* ]]; then
  echo "    Verified via CLI: container is running"
else
  echo "    ERROR: Container not running in CLI: $CLI_STATE"
  kill -9 $SERVER_PID $AGENT_PID || true
  exit 1
fi

echo "[6] Crashing the Agent (kill -9 $AGENT_PID)..."
kill -9 $AGENT_PID
wait $AGENT_PID 2>/dev/null || true

echo "[7] Verifying container SURVIVED agent crash..."
sleep 2
CLI_STATE_AFTER=$($RUNTIME_BIN --root "$RUNTIME_ROOT" state "$ACTUAL_POD_ID" || echo "failed")
if [[ "$CLI_STATE_AFTER" == *"running"* ]]; then
  echo "    Success: Container is STILL running :)"
else
  echo "    ERROR: Container died with the agent!"
  kill -9 $SERVER_PID || true
  exit 1
fi

echo "[8] Restarting k3rs-agent..."
RUST_LOG=info cargo run --bin k3rs-agent -- \
  --server "http://127.0.0.1:6445" \
  --token "test-recovery-token" \
  --node-name "test-node" \
  --proxy-port 6446 \
  --service-proxy-port 10257 \
  --dns-port 5354 \
  >> "$AGENT_LOG" 2>&1 &
AGENT_PID2=$!

echo "[9] Wait for recovery procedure to complete..."
sleep 5

AGENT_ADOPT=$(grep "Agent recovery: adopting desired container $ACTUAL_POD_ID" "$AGENT_LOG" || echo "")
if [ -n "$AGENT_ADOPT" ]; then
  echo "    Success: Agent correctly adopted the surviving container!"
else
  echo "    ERROR: Agent failed to adopt the container. Logs:"
  tail -n 20 "$AGENT_LOG"
  kill -9 $SERVER_PID $AGENT_PID2 || true
  exit 1
fi

echo "=========================================================="
echo "✅ Recovery Test PASSED!"
echo "=========================================================="

kill -9 $SERVER_PID $AGENT_PID2 || true
rm -rf "$DATA_DIR"
