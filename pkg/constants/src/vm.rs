//! Virtual machine / guest constants.

/// vsock port used for exec communication between host and guest.
pub const VSOCK_EXEC_PORT: u32 = 5555;

/// virtiofs mount tag for the root filesystem.
pub const VIRTIOFS_TAG: &str = "rootfs";

/// Default hostname assigned to guest VMs.
pub const DEFAULT_VM_HOSTNAME: &str = "k3rs-guest";
