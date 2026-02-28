//! Container runtime constants.

/// Version of youki to download when not found in PATH.
pub const YOUKI_VERSION: &str = "v0.6.0";

/// Version of crun to download when not found in PATH.
pub const CRUN_VERSION: &str = "1.26";

/// Default OCI runtime name.
pub const DEFAULT_RUNTIME: &str = "youki";

/// All OCI runtimes supported by the installer.
pub const SUPPORTED_RUNTIMES: &[&str] = &["youki", "crun"];

/// Default vCPU count for micro-VMs.
pub const DEFAULT_CPU_COUNT: u32 = 1;

/// Default memory size in MiB for micro-VMs.
pub const DEFAULT_MEMORY_MB: u64 = 256;
