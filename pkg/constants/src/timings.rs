//! Timing constants for controllers, heartbeats, and backoff.

/// Heartbeat poll interval when connected (seconds).
pub const HEARTBEAT_INTERVAL_SECS: u64 = 10;

/// Heartbeat HTTP request timeout (seconds).
pub const HEARTBEAT_TIMEOUT_SECS: u64 = 5;

/// Reconnect loop idle sleep when already connected (seconds).
pub const RECONNECT_IDLE_SECS: u64 = 5;

/// DeploymentController reconciliation interval (seconds).
pub const DEPLOYMENT_CHECK_INTERVAL_SECS: u64 = 10;

/// EvictionController check interval (seconds).
pub const EVICTION_CHECK_INTERVAL_SECS: u64 = 30;

/// Grace period before evicting pods from an Unknown node (seconds).
pub const EVICTION_GRACE_PERIOD_SECS: u64 = 300;

/// HPAController reconciliation interval (seconds).
pub const HPA_CHECK_INTERVAL_SECS: u64 = 30;

/// CronJobController reconciliation interval (seconds).
pub const CRONJOB_CHECK_INTERVAL_SECS: u64 = 30;

/// Watchdog process-alive poll interval (seconds).
pub const WATCHDOG_POLL_INTERVAL_SECS: u64 = 2;

/// Maximum exponential backoff delay (seconds).
pub const BACKOFF_MAX_SECS: u64 = 30;

/// Maximum shift exponent for exponential backoff (prevents u64 overflow).
pub const BACKOFF_SHIFT_CAP: u32 = 30;

/// Default backup interval (seconds).
pub const DEFAULT_BACKUP_INTERVAL_SECS: u64 = 3600;

/// Default number of backup files to retain.
pub const DEFAULT_BACKUP_RETENTION: usize = 5;

/// Systemd restart delay (seconds).
pub const SYSTEMD_RESTART_SECS: u64 = 5;
