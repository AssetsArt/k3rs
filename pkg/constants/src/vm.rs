//! Virtual machine / guest constants.

/// vsock port used for exec communication between host and guest.
pub const VSOCK_EXEC_PORT: u32 = 5555;

/// virtiofs mount tag for the root filesystem.
pub const VIRTIOFS_TAG: &str = "rootfs";

/// Default hostname assigned to guest VMs.
pub const DEFAULT_VM_HOSTNAME: &str = "k3rs-guest";

/// Byte prefix that switches vsock exec into streaming PTY mode.
pub const VSOCK_STREAM_PREFIX: u8 = 0x01;
