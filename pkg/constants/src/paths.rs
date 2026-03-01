//! Filesystem path constants.
//!
//! Only three base directories — everything else is derived at the usage site.
//!
//! - `/etc/k3rs/`        — configuration files, TLS certificates
//! - `/var/lib/k3rs/`    — persistent data, state, binaries, runtime
//! - `/var/logs/k3rs/`   — log files

/// Base directory for all k3rs configuration files.
pub const CONFIG_DIR: &str = "/etc/k3rs";

/// Base directory for all k3rs persistent data (state, runtime, binaries, images).
pub const DATA_DIR: &str = "/var/lib/k3rs";

/// Base directory for all k3rs log files.
pub const LOG_DIR: &str = "/var/logs/k3rs";
