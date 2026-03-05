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

// ─── Controller intervals (remaining) ───────────────────────────

/// NodeController health-check interval (seconds).
pub const NODE_CHECK_INTERVAL_SECS: u64 = 15;

/// Heartbeat staleness threshold before marking a node NotReady (seconds).
pub const NODE_NOT_READY_THRESHOLD_SECS: u64 = 30;

/// Heartbeat staleness threshold before marking a node Unknown (seconds).
pub const NODE_UNKNOWN_THRESHOLD_SECS: u64 = 60;

/// DaemonSetController reconciliation interval (seconds).
pub const DAEMONSET_CHECK_INTERVAL_SECS: u64 = 15;

/// JobController reconciliation interval (seconds).
pub const JOB_CHECK_INTERVAL_SECS: u64 = 10;

/// ReplicaSetController reconciliation interval (seconds).
pub const REPLICASET_CHECK_INTERVAL_SECS: u64 = 10;

/// EndpointController reconciliation interval (seconds).
pub const ENDPOINT_CHECK_INTERVAL_SECS: u64 = 10;

/// VpcController reconciliation interval (seconds).
pub const VPC_CHECK_INTERVAL_SECS: u64 = 15;

/// RestoreWatcher poll interval (seconds).
pub const RESTORE_WATCHER_INTERVAL_SECS: u64 = 5;

// ─── Agent loop intervals ───────────────────────────────────────

/// Agent pod sync interval (seconds).
pub const POD_SYNC_INTERVAL_SECS: u64 = 5;

/// Agent image report interval (seconds).
pub const IMAGE_REPORT_INTERVAL_SECS: u64 = 30;

// ─── VM / container timeouts ────────────────────────────────────

/// Timeout for VM exec commands over IPC (seconds).
pub const VM_EXEC_TIMEOUT_SECS: u64 = 30;

/// Timeout for vsock connection establishment (seconds).
pub const VSOCK_CONNECT_TIMEOUT_SECS: u64 = 5;

/// VPC deletion cooldown period (seconds).
pub const VPC_DELETION_COOLDOWN_SECS: i64 = 300;
