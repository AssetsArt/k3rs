#!/usr/bin/env bash
set -euo pipefail

# k3rs Podman dev environment — Linux container for OCI + Firecracker testing
#
# Usage:
#   ./scripts/dev-podman.sh                    # interactive shell
#   ./scripts/dev-podman.sh --all              # server + agent via pm2 (KVM enabled)
#   ./scripts/dev-podman.sh --all --ui         # server + agent + UI via pm2
#   ./scripts/dev-podman.sh server             # run k3rs-server inside container
#   ./scripts/dev-podman.sh agent              # run k3rs-agent inside container
#   ./scripts/dev-podman.sh test               # run cargo test
#   ./scripts/dev-podman.sh build              # build only
#   ./scripts/dev-podman.sh --kvm              # enable KVM passthrough (for Firecracker)
#
# Environment:
#   K3RS_RUNTIME=youki|crun    OCI runtime to use (default: youki)
#   K3RS_KVM=1                 Enable KVM passthrough
#   K3RS_UI=1                  Enable UI (dx serve)

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

CONTAINER_NAME="k3rs-dev"
IMAGE_NAME="k3rs-dev"
ENABLE_KVM="${K3RS_KVM:-0}"
ENABLE_UI="${K3RS_UI:-0}"
RUNTIME="${K3RS_RUNTIME:-youki}"

# Parse flags first, then positional
ARGS=()
for arg in "$@"; do
    case "$arg" in
        --kvm)  ENABLE_KVM=1 ;;
        --ui)   ENABLE_UI=1 ;;
        --all)  ARGS+=("all") ; ENABLE_KVM=1 ;;
        *)      ARGS+=("$arg") ;;
    esac
done
MODE="${ARGS[0]:-shell}"

# ─── Colors ───────────────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
CYAN='\033[0;36m'
YELLOW='\033[1;33m'
NC='\033[0m'

# ─── Build image if needed ────────────────────────────────────────────
build_image() {
    if ! podman image exists "$IMAGE_NAME" 2>/dev/null; then
        echo -e "${CYAN}📦 Building k3rs-dev container image...${NC}"
        podman build -f "$PROJECT_ROOT/Containerfile.dev" -t "$IMAGE_NAME" "$PROJECT_ROOT"
    else
        echo -e "${GREEN}✅ Image '$IMAGE_NAME' exists${NC}"
    fi
}

# ─── Stop existing container ─────────────────────────────────────────
cleanup() {
    if podman container exists "$CONTAINER_NAME" 2>/dev/null; then
        echo -e "${YELLOW}🔄 Removing existing container '$CONTAINER_NAME'...${NC}"
        podman rm -f "$CONTAINER_NAME" 2>/dev/null || true
    fi
}

# ─── Build podman run args ────────────────────────────────────────────
build_run_args() {
    local args=(
        --name "$CONTAINER_NAME"
        --rm
        -it
        # Mount workspace
        -v "$PROJECT_ROOT:/workspace:z"
        # Persistent cargo cache (speeds up rebuilds)
        -v "k3rs-cargo-registry:/usr/local/cargo/registry"
        -v "k3rs-cargo-git:/usr/local/cargo/git"
        -v "k3rs-target:/workspace/target"
        # Runtime dirs
        -v "k3rs-data:/var/lib/k3rs"
        # Ports
        -p 6443:6443
        -p 6444:6444
        -p 10256:10256
        # DNS port 5353 is internal only (conflicts with macOS mDNS)
    )

    # UI port
    if [ "$ENABLE_UI" = "1" ]; then
        args+=(-p 8080:8080)
    fi

    args+=(
        # Environment
        -e "RUST_LOG=debug"
        -e "K3RS_RUNTIME=$RUNTIME"
        # Privileged mode — required for nested OCI containers
        # (mounting proc/sysfs, creating namespaces, cgroup management)
        --privileged
    )

    # KVM passthrough for Firecracker
    if [ "$ENABLE_KVM" = "1" ]; then
        if [ -e /dev/kvm ]; then
            args+=(--device /dev/kvm)
            echo -e "${GREEN}🔥 KVM passthrough enabled (/dev/kvm)${NC}" >&2
        else
            echo -e "${YELLOW}⚠ /dev/kvm not found — Firecracker will not be available${NC}" >&2
        fi
    fi

    echo "${args[@]}"
}

# ─── Commands inside container ────────────────────────────────────────
run_container() {
    local cmd="$1"
    local run_args
    run_args=$(build_run_args)

    case "$cmd" in
        shell)
            echo -e "${CYAN}🐚 Starting interactive shell in k3rs-dev container${NC}"
            echo -e "   Runtime: ${GREEN}$RUNTIME${NC}"
            echo -e "   KVM: $([ "$ENABLE_KVM" = "1" ] && echo -e "${GREEN}enabled${NC}" || echo -e "${YELLOW}disabled${NC}")"
            echo ""
            # shellcheck disable=SC2086
            podman run $run_args "$IMAGE_NAME" bash
            ;;

        all)
            echo -e "${CYAN}🚀 Starting full dev environment (server + agent) via pm2${NC}"
            echo -e "   Runtime: ${GREEN}$RUNTIME${NC}"
            echo -e "   KVM: $([ "$ENABLE_KVM" = "1" ] && echo -e "${GREEN}enabled${NC}" || echo -e "${YELLOW}disabled${NC}")"
            echo -e "   UI: $([ "$ENABLE_UI" = "1" ] && echo -e "${GREEN}enabled (port 8080)${NC}" || echo -e "${YELLOW}disabled${NC}")"
            echo ""
            # shellcheck disable=SC2086
            podman run -e K3RS_UI="$ENABLE_UI" $run_args "$IMAGE_NAME" bash -c '
                echo "🔨 Building workspace first..."
                cargo build --workspace 2>&1 | tail -3

                # Generate pm2 ecosystem config
                cat > /tmp/ecosystem.config.js <<EOF
module.exports = {
  apps: [
    {
      name: "server",
      script: "bash",
      args: "-c ./scripts/dev-server.sh",
      cwd: "/workspace",
      env: { RUST_LOG: "debug" },
    },
    {
      name: "agent",
      script: "bash",
      args: "-c ./scripts/dev-agent.sh",
      cwd: "/workspace",
      env: { RUST_LOG: "debug" },
      restart_delay: 5000,
    },
  ],
};
EOF

                # Add UI process if enabled
                if [ "${K3RS_UI:-0}" = "1" ]; then
                    cat >> /tmp/ecosystem.config.js <<UIEOF
module.exports.apps.push({
  name: "ui",
  script: "dx",
  args: "serve --addr 0.0.0.0 --port 8080",
  cwd: "/workspace/cmd/k3rs-ui",
});
UIEOF
                fi

                echo "🚀 Starting services via pm2..."
                pm2 start /tmp/ecosystem.config.js
                sleep 2
                pm2 status
                echo ""
                echo "📋 Commands: pm2 status | pm2 logs | pm2 restart <name> | pm2 stop all"
                echo ""
                exec bash
            '
            ;;

        server)
            echo -e "${CYAN}🚀 Starting k3rs-server in container${NC}"
            # shellcheck disable=SC2086
            podman run $run_args "$IMAGE_NAME" bash -c "scripts/dev-server.sh"
            ;;

        agent)
            echo -e "${CYAN}🤖 Starting k3rs-agent in container${NC}"
            # shellcheck disable=SC2086
            podman run $run_args "$IMAGE_NAME" bash -c "scripts/dev-agent.sh"
            ;;

        test)
            echo -e "${CYAN}🧪 Running cargo test in container${NC}"
            # shellcheck disable=SC2086
            podman run $run_args "$IMAGE_NAME" \
                cargo test --workspace
            ;;

        build)
            echo -e "${CYAN}🔨 Building workspace in container${NC}"
            # shellcheck disable=SC2086
            podman run $run_args "$IMAGE_NAME" \
                cargo build --workspace
            ;;

        check)
            echo -e "${CYAN}✔ Running cargo check in container${NC}"
            # shellcheck disable=SC2086
            podman run $run_args "$IMAGE_NAME" \
                cargo check --workspace
            ;;

        *)
            echo "Usage: $0 [shell|all|server|agent|test|build|check] [--kvm]"
            echo ""
            echo "Modes:"
            echo "  shell    Interactive bash shell (default)"
            echo "  all      Server + Agent via pm2 (KVM auto-enabled)"
            echo "  server   Run k3rs-server with cargo-watch"
            echo "  agent    Run k3rs-agent with cargo-watch"
            echo "  test     Run cargo test --workspace"
            echo "  build    Run cargo build --workspace"
            echo "  check    Run cargo check --workspace"
            echo ""
            echo "Flags:"
            echo "  --kvm    Passthrough /dev/kvm for Firecracker"
            echo "  --ui     Enable Dioxus UI (dx serve on port 8080)"
            echo "  --all    Same as 'all' mode"
            echo ""
            echo "Environment:"
            echo "  K3RS_RUNTIME=youki|crun   OCI runtime (default: youki)"
            echo "  K3RS_KVM=1                Enable KVM passthrough"
            echo "  K3RS_UI=1                 Enable UI"
            exit 1
            ;;
    esac
}

# ─── Main ─────────────────────────────────────────────────────────────
echo "╔══════════════════════════════════════════════════════════╗"
echo "║  k3rs Podman Dev Environment                           ║"
echo "╠══════════════════════════════════════════════════════════╣"
echo "║  Mode     : $MODE"
echo "║  Runtime  : $RUNTIME"
echo "║  KVM      : $([ "$ENABLE_KVM" = "1" ] && echo "enabled" || echo "disabled")"
echo "║  UI       : $([ "$ENABLE_UI" = "1" ] && echo "enabled" || echo "disabled")"
echo "╚══════════════════════════════════════════════════════════╝"
echo

build_image
cleanup
run_container "$MODE"
