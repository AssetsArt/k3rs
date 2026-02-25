use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Volume mount in a pod.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeMount {
    /// Name of the volume (must match a Volume in the pod spec)
    pub name: String,
    /// Path inside the container to mount the volume
    pub mount_path: String,
    /// Whether to mount read-only
    #[serde(default)]
    pub read_only: bool,
}

/// Volume source — where the storage comes from.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum VolumeSource {
    /// A path on the host node's filesystem
    HostPath { path: String },
    /// An empty directory created when the pod starts, deleted when it stops
    EmptyDir {},
    /// A persistent volume claim reference
    PersistentVolumeClaim { claim_name: String },
    /// A configmap projected as files
    ConfigMap { name: String },
    /// A secret projected as files
    Secret { secret_name: String },
}

/// Named volume in a pod spec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Volume {
    pub name: String,
    pub source: VolumeSource,
}

// --- Persistent Volume Claims ---

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AccessMode {
    ReadWriteOnce,
    ReadOnlyMany,
    ReadWriteMany,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum PVCPhase {
    #[default]
    Pending,
    Bound,
    Lost,
}

impl std::fmt::Display for PVCPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PVCPhase::Pending => write!(f, "Pending"),
            PVCPhase::Bound => write!(f, "Bound"),
            PVCPhase::Lost => write!(f, "Lost"),
        }
    }
}

/// Persistent Volume Claim — a request for storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistentVolumeClaim {
    pub id: String,
    pub name: String,
    pub namespace: String,
    /// Storage class name (e.g. "default", "fast-ssd")
    #[serde(default)]
    pub storage_class: Option<String>,
    /// Access modes
    #[serde(default)]
    pub access_modes: Vec<AccessMode>,
    /// Requested storage in bytes
    #[serde(default)]
    pub requested_bytes: u64,
    /// Current phase
    #[serde(default)]
    pub phase: PVCPhase,
    pub created_at: DateTime<Utc>,
}
