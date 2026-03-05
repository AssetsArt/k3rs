//! Virtual machine / guest constants.

/// vsock port used for exec communication between host and guest.
pub const VSOCK_EXEC_PORT: u32 = 5555;

/// virtiofs mount tag for the root filesystem.
pub const VIRTIOFS_TAG: &str = "rootfs";

/// Default hostname assigned to guest VMs.
pub const DEFAULT_VM_HOSTNAME: &str = "k3rs-guest";

/// Byte prefix that switches vsock exec into streaming PTY mode.
pub const VSOCK_STREAM_PREFIX: u8 = 0x01;

// ─── Guest filesystem paths ─────────────────────────────────────

/// Kernel binary filename inside the kernel directory.
pub const KERNEL_FILENAME: &str = "vmlinux";

/// Initrd image filename inside the kernel directory.
pub const INITRD_FILENAME: &str = "initrd.img";

/// Path where k3rs-init is injected inside the guest rootfs.
pub const GUEST_INIT_PATH: &str = "sbin/k3rs-init";

/// OCI config.json path inside guest rootfs (read by k3rs-init).
pub const GUEST_CONFIG_PATH: &str = "config.json";

/// Guest rootfs mount point (for initrd-mode chroot).
pub const GUEST_ROOTFS_MOUNT: &str = "/mnt/rootfs";

/// eBPF filesystem pin directory for VPC programs.
pub const BPFFS_PIN_DIR: &str = "/sys/fs/bpf/k3rs_vpc";
