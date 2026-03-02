//! # k3rs-init — Guest PID 1
//!
//! This binary runs as `/sbin/k3rs-init` inside lightweight Linux microVMs (both
//! macOS Virtualization.framework and Linux Firecracker/rust-vmm backends).
//!
//! ## Responsibilities
//! 1. Mount essential pseudo-filesystems (`/proc`, `/sys`, `/dev`, `/tmp`, `/run`)
//! 2. Setup basic networking (loopback `lo`, `eth0`)
//! 3. Set hostname
//! 4. Reap orphaned zombie processes (critical for PID 1)
//! 5. Parse OCI `config.json` and exec the container entrypoint
//!
//! ## Build
//! ```bash
//! # x86_64
//! cargo build --release --target x86_64-unknown-linux-musl -p k3rs-init
//! # aarch64
//! cargo build --release --target aarch64-unknown-linux-musl -p k3rs-init
//! ```
//!
//! ## Note
//! This binary is strictly Linux-only. On macOS it compiles with stubs for CI,
//! but should **never** be run outside a Linux guest VM.

// On non-Linux targets, all the Linux-specific code is cfg'd out,
// making types/constants appear unused. Suppress those warnings.
#![cfg_attr(not(target_os = "linux"), allow(unused))]

use std::io::Write;
use std::path::Path;
use std::sync::OnceLock;

// Modules
mod oci_spec;
#[macro_use]
mod logging;
mod container;
mod filesystem;
mod networking;
mod signals;
mod vsock;

// ============================================================
// Constants
// ============================================================

const DEFAULT_HOSTNAME: &str = "k3rs-guest";

/// virtio-fs tag used by k3rs-vmm for rootfs sharing.
const VIRTIOFS_TAG: &str = "rootfs";

/// vsock port for exec commands from host (k3rs-vmm).
const VSOCK_EXEC_PORT: u32 = 5555;

// ============================================================
// Boot mode detection
// ============================================================

/// True when k3rs-init is running from an initrd (must mount virtiofs + chroot).
/// False when the kernel has already mounted virtiofs as the root filesystem.
static INITRD_MODE: OnceLock<bool> = OnceLock::new();

fn is_initrd_mode() -> bool {
    *INITRD_MODE.get().unwrap_or(&false)
}

// ============================================================
// Main
// ============================================================

fn main() {
    log_info!("starting (pid={})", std::process::id());

    // Steps 1-5 are Linux-only
    #[cfg(target_os = "linux")]
    linux_main();

    #[cfg(not(target_os = "linux"))]
    {
        log_error!("k3rs-init is a Linux-only binary — cannot run on this platform");
        std::process::exit(1);
    }
}

#[cfg(target_os = "linux")]
fn linux_main() {
    // 1. Mount pseudo-filesystems
    if let Err(e) = filesystem::mount_filesystems() {
        log_error!("failed to mount filesystems: {}", e);
    }

    // Detect boot mode
    let initrd_mode = !Path::new("/config.json").exists();
    let _ = INITRD_MODE.set(initrd_mode);

    // 1b. Mount virtio-fs shared rootfs from host (initrd mode only)
    if initrd_mode {
        filesystem::mount_virtiofs();
    } else {
        log_info!("no-initrd mode: virtiofs is already the root filesystem");
    }

    // 2. Set hostname
    if let Err(e) = nix::unistd::sethostname(DEFAULT_HOSTNAME) {
        log_error!("failed to set hostname: {}", e);
    } else {
        log_info!("hostname set to '{}'", DEFAULT_HOSTNAME);
    }

    // 3. Setup networking
    if let Err(e) = networking::setup_networking() {
        log_error!("failed to setup networking: {}", e);
    }

    // 4. Install signal handlers
    signals::install_signal_handlers();

    // 4b. Start vsock exec listener (background thread)
    vsock::start_vsock_listener();

    // 5. Parse OCI config and execute entrypoint
    match filesystem::load_oci_config() {
        Ok(spec) => {
            // Override hostname if OCI spec provides one
            if let Some(ref h) = spec.hostname {
                if h != DEFAULT_HOSTNAME {
                    let _ = nix::unistd::sethostname(h);
                }
            }

            if let Some(process) = spec.process {
                container::run_entrypoint(process);
            } else {
                log_error!("no 'process' section in OCI config — dropping to reaper loop");
                signals::reaper_loop();
            }
        }
        Err(e) => {
            log_error!("failed to load OCI config: {} — dropping to reaper loop", e);
            signals::reaper_loop();
        }
    }
}
