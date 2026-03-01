use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Current backup format version.
pub const BACKUP_VERSION: &str = "1.0";

/// A single key-value entry in the backup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupEntry {
    /// Registry key (e.g. "/registry/pods/default/my-pod").
    pub key: String,
    /// JSON value stored for this key.
    pub value: serde_json::Value,
}

/// PKI section of a backup: CA certificate for cluster identity restore.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BackupPki {
    /// CA certificate PEM.
    pub ca_cert: String,
}

/// Full cluster backup file.
/// Stored as gzip-compressed JSON on disk (`*.k3rs-backup.json.gz`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupFile {
    /// Format version — must equal BACKUP_VERSION.
    pub version: String,
    /// When this backup was created.
    pub created_at: DateTime<Utc>,
    /// Logical cluster name (informational).
    pub cluster_name: String,
    /// Number of registered nodes at snapshot time.
    pub node_count: usize,
    /// Total number of registry entries.
    pub key_count: usize,
    /// All registry key-value pairs.
    pub entries: Vec<BackupEntry>,
    /// PKI material exported with the backup.
    pub pki: BackupPki,
}

/// Summary of the most recent backup; stored at `/registry/_backup/last`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupStatus {
    pub last_backup_at: Option<DateTime<Utc>>,
    pub last_backup_file: Option<String>,
    pub key_count: Option<usize>,
    pub status: String,
}
