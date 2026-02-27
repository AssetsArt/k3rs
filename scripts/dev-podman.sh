#!/usr/bin/env bash
set -euo pipefail

# k3rs Podman dev environment — Linux container for OCI + Firecracker testing
#
# Usage:
#   ./scripts/dev-podman.sh                    # interactive shell
#   ./scripts/dev-podman.sh --all              # server + agent in tmux (KVM enabled)
#   ./scripts/dev-podman.sh server             # run k3rs-server inside container
#   ./scripts/dev-podman.sh agent              # run k3rs-agent inside container
#   ./scripts/dev-podman.sh test               # run cargo test
#   ./scripts/dev-podman.sh build              # build only
#   ./scripts/dev-podman.sh --kvm              # enable KVM passthrough (for Firecracker)
#
# Environment:
#   K3RS_RUNTIME=youki|crun    OCI runtime to use (default: youki)
#   K3RS_KVM=1                 Enable KVM passthrough

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

CONTAINER_NAME="k3rs-dev"
IMAGE_NAME="k3rs-dev"
ENABLE_KVM="${K3RS_KVM:-0}"
RUNTIME="${K3RS_RUNTIME:-youki}"

# Parse flags first, then positional
ARGS=()
for arg in "$@"; do
    case "$arg" in
        --kvm)  ENABLE_KVM=1 ;;
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
        -p 5353:5353/udp
        # Environment
        -e "RUST_LOG=debug"
        -e "K3RS_RUNTIME=$RUNTIME"
        # Capabilities for container runtime
        --cap-add SYS_ADMIN
        --cap-add NET_ADMIN
        --cap-add SYS_PTRACE
        --security-opt seccomp=unconfined
        --security-opt apparmor=unconfined
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
            echo -e "${CYAN}🚀 Starting full dev environment (server + agent) in tmux${NC}"
            echo -e "   Runtime: ${GREEN}$RUNTIME${NC}"
            echo -e "   KVM: $([ "$ENABLE_KVM" = "1" ] && echo -e "${GREEN}enabled${NC}" || echo -e "${YELLOW}disabled${NC}")"
            echo ""
            # shellcheck disable=SC2086
            podman run $run_args "$IMAGE_NAME" bash -c '
                echo "🔨 Building workspace first..."
                cargo build --workspace 2>&1 | tail -3

                SESSION="k3rs"
                tmux new-session -d -s "$SESSION" -n server \
                    "RUST_LOG=debug cargo watch \
                        -x \"run --bin k3rs-server -- --port 6443 --token demo-token-123 --data-dir /var/lib/k3rs/data --node-name master-1\" \
                        -w pkg/ -w cmd/k3rs-server -i \"target/*\""

                # Wait for server to start
                sleep 3

                tmux split-window -h -t "$SESSION" \
                    "RUST_LOG=debug cargo watch \
                        -x \"run --bin k3rs-agent -- --server http://127.0.0.1:6443 --token demo-token-123 --node-name node-1 --proxy-port 6444 --service-proxy-port 10256 --dns-port 5353\" \
                        -w pkg/ -w cmd/k3rs-agent -i \"target/*\""

                tmux select-pane -t 0
                exec tmux attach-session -t "$SESSION"
            '
            ;;

        server)
            echo -e "${CYAN}🚀 Starting k3rs-server in container${NC}"
            # shellcheck disable=SC2086
            podman run $run_args "$IMAGE_NAME" \
                cargo watch \
                    -x "run --bin k3rs-server -- --port 6443 --token demo-token-123 --data-dir /var/lib/k3rs/data --node-name master-1" \
                    -w pkg/ -w cmd/k3rs-server -i "target/*"
            ;;

        agent)
            echo -e "${CYAN}🤖 Starting k3rs-agent in container${NC}"
            # shellcheck disable=SC2086
            podman run $run_args "$IMAGE_NAME" \
                cargo watch \
                    -x "run --bin k3rs-agent -- --server http://127.0.0.1:6443 --token demo-token-123 --node-name node-1 --proxy-port 6444 --service-proxy-port 10256 --dns-port 5353" \
                    -w pkg/ -w cmd/k3rs-agent -i "target/*"
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
            echo "  all      Server + Agent in tmux (KVM auto-enabled)"
            echo "  server   Run k3rs-server with cargo-watch"
            echo "  agent    Run k3rs-agent with cargo-watch"
            echo "  test     Run cargo test --workspace"
            echo "  build    Run cargo build --workspace"
            echo "  check    Run cargo check --workspace"
            echo ""
            echo "Flags:"
            echo "  --kvm    Passthrough /dev/kvm for Firecracker"
            echo "  --all    Same as 'all' mode"
            echo ""
            echo "Environment:"
            echo "  K3RS_RUNTIME=youki|crun   OCI runtime (default: youki)"
            echo "  K3RS_KVM=1                Enable KVM passthrough"
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
echo "╚══════════════════════════════════════════════════════════╝"
echo

build_image
cleanup
run_container "$MODE"
