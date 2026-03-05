#!/bin/bash
# Build k3rs release binaries for the current platform.
#
# Usage:
#   ./scripts/build-release.sh                    # build all components for current platform
#   ./scripts/build-release.sh --components server agent ctl   # build specific components
#   ./scripts/build-release.sh --output dist      # custom output directory
#   ./scripts/build-release.sh --target aarch64-unknown-linux-gnu  # cross-compile
#
# Components:
#   server   - k3rs-server (control plane)
#   agent    - k3rs-agent (node agent)
#   ctl      - k3rsctl (CLI tool)
#   ui       - k3rs-ui (web dashboard)
#   vpc      - k3rs-vpc (VPC daemon, Linux only)
#   init     - k3rs-init (guest init, Linux musl static)
#   vmm      - k3rs-vmm (VM manager, macOS only)
#
# Output:
#   <output_dir>/<binary>           – stripped release binaries
#   <output_dir>/checksums.sha256   – SHA256 checksums

set -euo pipefail

# ─── Configuration ────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
OUTPUT_DIR="dist"
TARGET=""
COMPONENTS=()
JOBS=""

# ─── Colors ───────────────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
NC='\033[0m'

log()   { echo -e "${CYAN}[build-release]${NC} $*"; }
ok()    { echo -e "${GREEN}[build-release]${NC} $*"; }
warn()  { echo -e "${YELLOW}[build-release]${NC} $*"; }
error() { echo -e "${RED}[build-release]${NC} $*" >&2; }

# ─── Parse arguments ─────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case "$1" in
        --output|-o)
            OUTPUT_DIR="$2"; shift 2
            ;;
        --target|-t)
            TARGET="$2"; shift 2
            ;;
        --components|-c)
            shift
            while [[ $# -gt 0 && ! "$1" =~ ^-- ]]; do
                COMPONENTS+=("$1"); shift
            done
            ;;
        --jobs|-j)
            JOBS="$2"; shift 2
            ;;
        --help|-h)
            head -20 "$0" | tail -19 | sed 's/^# \?//'
            exit 0
            ;;
        *)
            error "Unknown option: $1"
            exit 1
            ;;
    esac
done

# ─── Platform detection ──────────────────────────────────────────────
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
    Linux)  PLATFORM="linux" ;;
    Darwin) PLATFORM="macos" ;;
    *)      error "Unsupported OS: $OS"; exit 1 ;;
esac

case "$ARCH" in
    x86_64|amd64)   ARCH_LABEL="amd64" ;;
    aarch64|arm64)  ARCH_LABEL="arm64" ;;
    *)              error "Unsupported architecture: $ARCH"; exit 1 ;;
esac

log "Platform: ${PLATFORM}/${ARCH_LABEL}"

# ─── Determine components to build ───────────────────────────────────
if [[ ${#COMPONENTS[@]} -eq 0 ]]; then
    # Default: build platform-appropriate components
    COMPONENTS=(server agent ctl ui)
    if [[ "$PLATFORM" == "linux" ]]; then
        COMPONENTS+=(vpc init)
    elif [[ "$PLATFORM" == "macos" ]]; then
        COMPONENTS+=(vmm)
    fi
fi

# Map component names to cargo packages and binary names
declare -A PKG_MAP=(
    [server]="k3rs-server"
    [agent]="k3rs-agent"
    [ctl]="k3rsctl"
    [ui]="k3rs-ui"
    [vpc]="k3rs-vpc"
    [init]="k3rs-init"
    [vmm]="k3rs-vmm"
)

declare -A BIN_MAP=(
    [server]="k3rs-server"
    [agent]="k3rs-agent"
    [ctl]="k3rsctl"
    [ui]="k3rs-ui"
    [vpc]="k3rs-vpc"
    [init]="k3rs-init"
    [vmm]="k3rs-vmm"
)

# ─── Validate components ─────────────────────────────────────────────
for comp in "${COMPONENTS[@]}"; do
    if [[ -z "${PKG_MAP[$comp]:-}" ]]; then
        error "Unknown component: $comp"
        error "Valid components: server agent ctl ui vpc init vmm"
        exit 1
    fi

    # Platform checks
    if [[ "$comp" == "vmm" && "$PLATFORM" != "macos" ]]; then
        warn "Skipping k3rs-vmm (macOS only)"
        COMPONENTS=("${COMPONENTS[@]/$comp}")
    fi
    if [[ "$comp" == "init" && "$PLATFORM" != "linux" ]]; then
        warn "Skipping k3rs-init (Linux only)"
        COMPONENTS=("${COMPONENTS[@]/$comp}")
    fi
    if [[ "$comp" == "vpc" && "$PLATFORM" != "linux" ]]; then
        warn "Skipping k3rs-vpc (Linux only)"
        COMPONENTS=("${COMPONENTS[@]/$comp}")
    fi
done

# Remove empty entries
COMPONENTS=(${COMPONENTS[@]})

if [[ ${#COMPONENTS[@]} -eq 0 ]]; then
    error "No components to build"
    exit 1
fi

log "Components: ${COMPONENTS[*]}"

# ─── Setup output directory ──────────────────────────────────────────
OUTPUT_PATH="$PROJECT_ROOT/$OUTPUT_DIR"
mkdir -p "$OUTPUT_PATH"

# ─── Build arguments ─────────────────────────────────────────────────
CARGO_ARGS=(--release)
if [[ -n "$TARGET" ]]; then
    CARGO_ARGS+=(--target "$TARGET")
fi
if [[ -n "$JOBS" ]]; then
    CARGO_ARGS+=(--jobs "$JOBS")
fi

# Determine target directory
if [[ -n "$TARGET" ]]; then
    TARGET_DIR="$PROJECT_ROOT/target/$TARGET/release"
else
    TARGET_DIR="$PROJECT_ROOT/target/release"
fi

# ─── Build each component ────────────────────────────────────────────
BUILT_BINARIES=()
FAILED=()

for comp in "${COMPONENTS[@]}"; do
    pkg="${PKG_MAP[$comp]}"
    bin="${BIN_MAP[$comp]}"

    log "Building ${pkg}..."

    # Special handling for k3rs-init (musl static linking)
    if [[ "$comp" == "init" && -z "$TARGET" ]]; then
        INIT_TARGET="$(uname -m)-unknown-linux-musl"
        INIT_TARGET_DIR="$PROJECT_ROOT/target/$INIT_TARGET/release"

        if ! cargo build -p "$pkg" --release --target "$INIT_TARGET" 2>&1; then
            error "Failed to build $pkg"
            FAILED+=("$comp")
            continue
        fi

        if [[ -f "$INIT_TARGET_DIR/$bin" ]]; then
            cp "$INIT_TARGET_DIR/$bin" "$OUTPUT_PATH/$bin"
            BUILT_BINARIES+=("$bin")
            ok "Built $bin (musl static, target: $INIT_TARGET)"
        else
            error "Binary not found: $INIT_TARGET_DIR/$bin"
            FAILED+=("$comp")
        fi
        continue
    fi

    if ! cargo build -p "$pkg" "${CARGO_ARGS[@]}" 2>&1; then
        error "Failed to build $pkg"
        FAILED+=("$comp")
        continue
    fi

    if [[ -f "$TARGET_DIR/$bin" ]]; then
        cp "$TARGET_DIR/$bin" "$OUTPUT_PATH/$bin"
        BUILT_BINARIES+=("$bin")
        ok "Built $bin"
    else
        error "Binary not found: $TARGET_DIR/$bin"
        FAILED+=("$comp")
    fi
done

# ─── Strip binaries ──────────────────────────────────────────────────
log "Stripping binaries..."
for bin in "${BUILT_BINARIES[@]}"; do
    bin_path="$OUTPUT_PATH/$bin"
    if file "$bin_path" | grep -q "not stripped"; then
        strip "$bin_path" 2>/dev/null || warn "Could not strip $bin"
    fi
done

# ─── Sign macOS VMM binary ───────────────────────────────────────────
if [[ "$PLATFORM" == "macos" ]]; then
    VMM_BIN="$OUTPUT_PATH/k3rs-vmm"
    ENTITLEMENTS="$PROJECT_ROOT/cmd/k3rs-vmm/k3rs-vmm.entitlements"
    if [[ -f "$VMM_BIN" && -f "$ENTITLEMENTS" ]]; then
        log "Signing k3rs-vmm with virtualization entitlement..."
        codesign --entitlements "$ENTITLEMENTS" --force -s - "$VMM_BIN"
        ok "Signed k3rs-vmm"
    fi
fi

# ─── Generate checksums ──────────────────────────────────────────────
log "Generating checksums..."
CHECKSUM_FILE="$OUTPUT_PATH/checksums.sha256"
cd "$OUTPUT_PATH"

if command -v sha256sum &>/dev/null; then
    sha256sum "${BUILT_BINARIES[@]}" > checksums.sha256
elif command -v shasum &>/dev/null; then
    shasum -a 256 "${BUILT_BINARIES[@]}" > checksums.sha256
fi

cd "$PROJECT_ROOT"

# ─── Summary ─────────────────────────────────────────────────────────
echo ""
log "═══════════════════════════════════════════════════"
log "  Build Release Summary"
log "═══════════════════════════════════════════════════"
echo ""

for bin in "${BUILT_BINARIES[@]}"; do
    size=$(du -h "$OUTPUT_PATH/$bin" | cut -f1)
    ok "  $bin  ($size)"
done

if [[ ${#FAILED[@]} -gt 0 ]]; then
    echo ""
    for comp in "${FAILED[@]}"; do
        error "  FAILED: ${PKG_MAP[$comp]}"
    done
fi

echo ""
log "Output: $OUTPUT_PATH/"
log "Checksums: $OUTPUT_PATH/checksums.sha256"

if [[ ${#FAILED[@]} -gt 0 ]]; then
    error "${#FAILED[@]} component(s) failed to build"
    exit 1
fi

ok "All ${#BUILT_BINARIES[@]} components built successfully"
