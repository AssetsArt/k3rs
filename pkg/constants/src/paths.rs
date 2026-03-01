//! Filesystem path constants.

// ─── Server ────────────────────────────────────────────────────────────────

/// Default config file path for the server.
pub const DEFAULT_SERVER_CONFIG: &str = "/etc/k3rs/config.yaml";

/// Default data directory for the server state store.
pub const DEFAULT_SERVER_DATA_DIR: &str = "/tmp/k3rs-data";

// ─── Agent ────────────────────────────────────────────────────────────────

/// Default config file path for the agent.
pub const DEFAULT_AGENT_CONFIG: &str = "/etc/k3rs/agent-config.yaml";

/// Directory prefix for per-node TLS certificate storage.
/// Full path = `AGENT_CERT_DIR_PREFIX + node_name`.
pub const AGENT_CERT_DIR_PREFIX: &str = "/tmp/k3rs-agent-";

// ─── Container runtime ────────────────────────────────────────────────────

/// Default container runtime data directory (rootfs, logs, state).
pub const DEFAULT_RUNTIME_DATA_DIR: &str = "/tmp/k3rs-runtime";

/// Preferred system-wide install directory for downloaded OCI runtimes.
pub const RUNTIME_INSTALL_DIR: &str = "/usr/local/bin/k3rs-runtime";

/// Per-user fallback install directory for downloaded OCI runtimes.
pub const RUNTIME_FALLBACK_DIR: &str = ".k3rs/bin";

// ─── Kernel / VM ──────────────────────────────────────────────────────────

/// Directory that holds guest kernel and initrd images.
pub const KERNEL_DIR: &str = "/var/lib/k3rs";

/// Filename of the guest kernel image inside `KERNEL_DIR`.
pub const KERNEL_FILENAME: &str = "vmlinux";

/// Filename of the guest initrd image inside `KERNEL_DIR`.
pub const INITRD_FILENAME: &str = "initrd.img";

/// Directory where VMM UNIX sockets are created.
pub const VMM_SOCKET_DIR: &str = "/tmp/k3rs-runtime/vms";

/// Directory for VMM PID files (one per running VM).
pub const VMM_PID_DIR: &str = "/tmp/k3rs-runtime/vms";

// ─── k3rs-init binary ─────────────────────────────────────────────────────

/// System-wide install path for the k3rs-init guest binary.
pub const K3RS_INIT_SYSTEM_PATH: &str = "/var/lib/k3rs/k3rs-init";

/// User-local install path for the k3rs-init guest binary.
pub const K3RS_INIT_USER_PATH: &str = ".k3rs/bin/k3rs-init";

/// Path where k3rs-init is injected inside the guest rootfs.
/// Using a dedicated name avoids overwriting a container's own /sbin/k3rs-init.
pub const GUEST_INIT_PATH: &str = "sbin/k3rs-init";

/// Standard config.json path inside guest rootfs (read by k3rs-init).
pub const GUEST_CONFIG_PATH: &str = "config.json";
