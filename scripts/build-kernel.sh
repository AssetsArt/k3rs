#!/bin/bash
# Build a minimal Linux kernel + initrd for k3rs microVMs.
#
# On macOS this runs the entire build inside a Docker container.
# On Linux it builds natively (requires gcc cross-compiler tools).
#
# Usage:
#   ./scripts/build-kernel.sh                  # auto-detect arch
#   ./scripts/build-kernel.sh --arch arm64     # force arm64
#   ./scripts/build-kernel.sh --arch amd64     # force amd64
#   ./scripts/build-kernel.sh --output /var/lib/k3rs  # custom output dir
#
# Output:
#   <output_dir>/vmlinux      – uncompressed Linux kernel
#   <output_dir>/initrd.img   – initrd containing k3rs-init as /sbin/init

set -euo pipefail

# ─── Configuration ────────────────────────────────────────────────────
KERNEL_VERSION="${KERNEL_VERSION:-6.12}"
KERNEL_MAJOR="v6.x"
KERNEL_URL="https://cdn.kernel.org/pub/linux/kernel/${KERNEL_MAJOR}/linux-${KERNEL_VERSION}.tar.xz"

OUTPUT_DIR="build/kernel"
TARGET_ARCH=""

# ─── Parse arguments ─────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case "$1" in
        --arch)   TARGET_ARCH="$2"; shift 2 ;;
        --output) OUTPUT_DIR="$2"; shift 2 ;;
        --help|-h)
            sed -n '2,15p' "$0" | sed 's/^# \?//'
            exit 0
            ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

# ─── Detect host architecture ────────────────────────────────────────
detect_arch() {
    local hw
    hw="$(uname -m)"
    case "$hw" in
        arm64|aarch64) echo "arm64" ;;
        x86_64|amd64)  echo "amd64" ;;
        *) echo "Unsupported architecture: $hw" >&2; exit 1 ;;
    esac
}

if [ -z "$TARGET_ARCH" ]; then
    TARGET_ARCH="$(detect_arch)"
fi

# Map our arch names to kernel/rust conventions
case "$TARGET_ARCH" in
    arm64)
        KERNEL_ARCH="arm64"
        CROSS_COMPILE="aarch64-linux-gnu-"
        KERNEL_IMAGE="arch/arm64/boot/Image"
        RUST_TARGET="aarch64-unknown-linux-musl"
        ;;
    amd64)
        KERNEL_ARCH="x86_64"
        CROSS_COMPILE=""
        KERNEL_IMAGE="arch/x86/boot/bzImage"
        RUST_TARGET="x86_64-unknown-linux-musl"
        ;;
    *)
        echo "Unsupported target arch: $TARGET_ARCH" >&2
        exit 1
        ;;
esac

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

echo "╔══════════════════════════════════════════════════════════╗"
echo "║  k3rs kernel builder                                    ║"
echo "╠══════════════════════════════════════════════════════════╣"
echo "║  Kernel  : linux-${KERNEL_VERSION} (${KERNEL_ARCH})"
echo "║  Output  : ${OUTPUT_DIR}"
echo "╚══════════════════════════════════════════════════════════╝"
echo

# ─── macOS → Docker path ─────────────────────────────────────────────
if [ "$(uname -s)" = "Darwin" ]; then
    echo "[build-kernel] macOS detected — building inside Docker..."

    if ! command -v docker &>/dev/null; then
        echo "[build-kernel] ERROR: Docker is required on macOS. Install Docker Desktop first." >&2
        exit 1
    fi

    # Build everything inside a Debian container
    docker build --load -t k3rs-kernel-builder -f - "$PROJECT_ROOT" <<'DOCKERFILE'
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential bc flex bison libelf-dev libssl-dev \
    gcc-aarch64-linux-gnu \
    musl-tools \
    curl xz-utils cpio ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Install Rust + cross targets
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y \
    --default-toolchain stable \
    --profile minimal
ENV PATH="/root/.cargo/bin:${PATH}"
RUN rustup target add aarch64-unknown-linux-musl x86_64-unknown-linux-musl

# Install zig (for cargo-zigbuild musl cross-compilation)
RUN mkdir -p /opt/zig \
    && curl -fSL "https://ziglang.org/download/0.13.0/zig-linux-$(uname -m)-0.13.0.tar.xz" \
       | tar -xJ -C /opt/zig --strip-components=1 \
    && ln -s /opt/zig/zig /usr/local/bin/zig \
    && zig version
RUN cargo install cargo-zigbuild

WORKDIR /build
DOCKERFILE

    mkdir -p "${PROJECT_ROOT}/${OUTPUT_DIR}"
    docker run --rm \
        -v "${PROJECT_ROOT}:/src:ro" \
        -v "${PROJECT_ROOT}/${OUTPUT_DIR}:/output" \
        -e KERNEL_VERSION="${KERNEL_VERSION}" \
        -e TARGET_ARCH="${TARGET_ARCH}" \
        k3rs-kernel-builder \
        bash -c '
set -euo pipefail

# ── Map arch ──
case "$TARGET_ARCH" in
    arm64)
        KERNEL_ARCH="arm64"
        CROSS_COMPILE="aarch64-linux-gnu-"
        KERNEL_IMAGE="arch/arm64/boot/Image"
        RUST_TARGET="aarch64-unknown-linux-musl"
        ;;
    amd64)
        KERNEL_ARCH="x86_64"
        CROSS_COMPILE=""
        KERNEL_IMAGE="arch/x86/boot/bzImage"
        RUST_TARGET="x86_64-unknown-linux-musl"
        ;;
esac

KERNEL_MAJOR="v6.x"
KERNEL_URL="https://cdn.kernel.org/pub/linux/kernel/${KERNEL_MAJOR}/linux-${KERNEL_VERSION}.tar.xz"

echo "[build-kernel] Step 1/4: Downloading linux-${KERNEL_VERSION} (~200MB)..."
cd /tmp
if [ ! -d "linux-${KERNEL_VERSION}" ]; then
    curl -fL# "$KERNEL_URL" | tar xJ
fi

cd "linux-${KERNEL_VERSION}"

echo "[build-kernel] Step 2/4: Configuring kernel (${KERNEL_ARCH}, minimal + virtio)..."
make ARCH="$KERNEL_ARCH" defconfig

# Enable virtio drivers needed for k3rs microVMs
scripts/config \
    -e VIRTIO -e VIRTIO_PCI -e VIRTIO_MMIO \
    -e VIRTIO_BLK -e VIRTIO_NET -e VIRTIO_CONSOLE \
    -e VIRTIOFS -e FUSE \
    -e VIRTIO_VSOCKETS -e VSOCKETS -e VHOST_VSOCK \
    -e NET -e INET -e EXT4_FS -e TMPFS \
    -e DEVTMPFS -e DEVTMPFS_MOUNT \
    -e PROC_FS -e SYSFS \
    -e OVERLAY_FS \
    -e CGROUPS -e CGROUP_CPUACCT -e CGROUP_DEVICE \
    -e CGROUP_FREEZER -e CGROUP_SCHED -e MEMCG -e CGROUP_PIDS \
    -e NAMESPACES -e NET_NS -e PID_NS -e USER_NS -e IPC_NS -e UTS_NS \
    -d MODULES -d SOUND -d DRM -d USB_SUPPORT -d WIRELESS \
    -d BLUETOOTH -d NFC -d INPUT_JOYSTICK -d INPUT_TABLET

make ARCH="$KERNEL_ARCH" olddefconfig

echo "[build-kernel] Step 3/4: Building kernel (this may take a while)..."
make ARCH="$KERNEL_ARCH" CROSS_COMPILE="$CROSS_COMPILE" -j"$(nproc)" Image 2>&1 | tail -5

echo "[build-kernel] Step 4/4: Building k3rs-init + creating initrd..."
# Copy source and build k3rs-init
cp -r /src /tmp/k3rs-src
cd /tmp/k3rs-src
cargo zigbuild --release --target "$RUST_TARGET" -p k3rs-init

# Create initrd
INITRD_ROOT="/tmp/initrd"
rm -rf "$INITRD_ROOT"
mkdir -p "$INITRD_ROOT"/{sbin,dev,proc,sys,tmp,run,mnt/rootfs,etc}
cp "target/${RUST_TARGET}/release/k3rs-init" "$INITRD_ROOT/sbin/init"
chmod 755 "$INITRD_ROOT/sbin/init"

cd "$INITRD_ROOT"
find . | cpio -o -H newc --quiet | gzip > /tmp/initrd.img

# Copy outputs
mkdir -p /output
cp "/tmp/linux-${KERNEL_VERSION}/${KERNEL_IMAGE}" /output/vmlinux
cp /tmp/initrd.img /output/initrd.img

echo
echo "✅ Kernel: /output/vmlinux ($(du -h /output/vmlinux | cut -f1))"
echo "✅ Initrd: /output/initrd.img ($(du -h /output/initrd.img | cut -f1))"
'

    echo
    echo "[build-kernel] ✅ Done! Output at: ${OUTPUT_DIR}/"
    ls -lh "${OUTPUT_DIR}"/vmlinux "${OUTPUT_DIR}"/initrd.img 2>/dev/null || true
    exit 0
fi

# ─── Linux native path ───────────────────────────────────────────────
echo "[build-kernel] Linux detected — building natively..."

# Check dependencies
for cmd in make gcc curl cpio flex bison bc; do
    if ! command -v "$cmd" &>/dev/null; then
        echo "[build-kernel] ERROR: '$cmd' is required. Install it first." >&2
        echo "[build-kernel] On Debian/Ubuntu: sudo apt-get install build-essential bc flex bison libelf-dev libssl-dev curl cpio" >&2
        echo "[build-kernel] On Fedora/RHEL:   sudo dnf install gcc make bc flex bison elfutils-libelf-devel openssl-devel curl cpio" >&2
        exit 1
    fi
done

if [ "$TARGET_ARCH" = "arm64" ] && [ "$(uname -m)" != "aarch64" ]; then
    if ! command -v aarch64-linux-gnu-gcc &>/dev/null; then
        echo "[build-kernel] ERROR: cross-compiler 'aarch64-linux-gnu-gcc' required. Install gcc-aarch64-linux-gnu." >&2
        exit 1
    fi
fi

BUILD_DIR="/tmp/k3rs-kernel-build"
mkdir -p "$BUILD_DIR"

# Step 1: Download kernel
echo "[build-kernel] Step 1/4: Downloading linux-${KERNEL_VERSION}..."
cd "$BUILD_DIR"
if [ ! -d "linux-${KERNEL_VERSION}" ]; then
    curl -sSL "$KERNEL_URL" | tar xJ
fi

cd "linux-${KERNEL_VERSION}"

# Step 2: Configure
echo "[build-kernel] Step 2/4: Configuring kernel (${KERNEL_ARCH}, minimal + virtio)..."
make ARCH="$KERNEL_ARCH" defconfig

scripts/config \
    -e VIRTIO -e VIRTIO_PCI -e VIRTIO_MMIO \
    -e VIRTIO_BLK -e VIRTIO_NET -e VIRTIO_CONSOLE \
    -e VIRTIOFS -e FUSE \
    -e VIRTIO_VSOCKETS -e VSOCKETS -e VHOST_VSOCK \
    -e NET -e INET -e EXT4_FS -e TMPFS \
    -e DEVTMPFS -e DEVTMPFS_MOUNT \
    -e PROC_FS -e SYSFS \
    -e OVERLAY_FS \
    -e CGROUPS -e CGROUP_CPUACCT -e CGROUP_DEVICE \
    -e CGROUP_FREEZER -e CGROUP_SCHED -e MEMCG -e CGROUP_PIDS \
    -e NAMESPACES -e NET_NS -e PID_NS -e USER_NS -e IPC_NS -e UTS_NS \
    -d MODULES -d SOUND -d DRM -d USB_SUPPORT -d WIRELESS \
    -d BLUETOOTH -d NFC -d INPUT_JOYSTICK -d INPUT_TABLET

make ARCH="$KERNEL_ARCH" olddefconfig

# Step 3: Build kernel
echo "[build-kernel] Step 3/4: Building kernel (this may take a while)..."
make ARCH="$KERNEL_ARCH" CROSS_COMPILE="$CROSS_COMPILE" -j"$(nproc)"

# Step 4: Build initrd
echo "[build-kernel] Step 4/4: Building k3rs-init + creating initrd..."
cd "$PROJECT_ROOT"

# Build k3rs-init
if command -v cargo-zigbuild &>/dev/null; then
    cargo zigbuild --release --target "$RUST_TARGET" -p k3rs-init
else
    cargo build --release --target "$RUST_TARGET" -p k3rs-init
fi

# Create initrd
INITRD_ROOT="/tmp/k3rs-initrd-$$"
rm -rf "$INITRD_ROOT"
mkdir -p "$INITRD_ROOT"/{sbin,dev,proc,sys,tmp,run,mnt/rootfs,etc}
cp "target/${RUST_TARGET}/release/k3rs-init" "$INITRD_ROOT/sbin/init"
chmod 755 "$INITRD_ROOT/sbin/init"

cd "$INITRD_ROOT"
find . | cpio -o -H newc --quiet | gzip > /tmp/initrd.img

# Install
mkdir -p "$OUTPUT_DIR"
cp "${BUILD_DIR}/linux-${KERNEL_VERSION}/${KERNEL_IMAGE}" "${OUTPUT_DIR}/vmlinux"
cp /tmp/initrd.img "${OUTPUT_DIR}/initrd.img"

# Cleanup
rm -rf "$INITRD_ROOT"

echo
echo "╔══════════════════════════════════════════════════════════╗"
echo "║  ✅ Build complete!                                     ║"
echo "╠══════════════════════════════════════════════════════════╣"
echo "║  Kernel : ${OUTPUT_DIR}/vmlinux ($(du -h "${OUTPUT_DIR}/vmlinux" | cut -f1))"
echo "║  Initrd : ${OUTPUT_DIR}/initrd.img ($(du -h "${OUTPUT_DIR}/initrd.img" | cut -f1))"
echo "╚══════════════════════════════════════════════════════════╝"
