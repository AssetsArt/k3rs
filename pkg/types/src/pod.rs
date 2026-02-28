use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// --- Resource requirements ---

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ResourceRequirements {
    /// CPU in millicores (1000 = 1 core)
    #[serde(default)]
    pub cpu_millis: u64,
    /// Memory in bytes
    #[serde(default)]
    pub memory_bytes: u64,
}

// --- Container spec ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerSpec {
    pub name: String,
    pub image: String,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub resources: ResourceRequirements,
    #[serde(default)]
    pub volume_mounts: Vec<crate::volume::VolumeMount>,
}

// --- Pod status ---

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PodStatus {
    Pending,
    Scheduled,
    ContainerCreating,
    Running,
    Succeeded,
    Failed,
    Unknown,
}

impl std::fmt::Display for PodStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PodStatus::Pending => write!(f, "Pending"),
            PodStatus::Scheduled => write!(f, "Scheduled"),
            PodStatus::ContainerCreating => write!(f, "ContainerCreating"),
            PodStatus::Running => write!(f, "Running"),
            PodStatus::Succeeded => write!(f, "Succeeded"),
            PodStatus::Failed => write!(f, "Failed"),
            PodStatus::Unknown => write!(f, "Unknown"),
        }
    }
}

// --- Pod spec ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PodSpec {
    pub containers: Vec<ContainerSpec>,
    /// Explicit runtime selection: "youki", "crun", "vm" (Apple VZ on macOS / Firecracker on Linux)
    #[serde(default)]
    pub runtime: Option<String>,
    #[serde(default)]
    pub node_affinity: HashMap<String, String>,
    #[serde(default)]
    pub tolerations: Vec<Toleration>,
    #[serde(default)]
    pub volumes: Vec<crate::volume::Volume>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Toleration {
    pub key: String,
    #[serde(default)]
    pub operator: TolerationOperator,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub effect: TaintEffect,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub enum TolerationOperator {
    #[default]
    Equal,
    Exists,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub enum TaintEffect {
    #[default]
    NoSchedule,
    PreferNoSchedule,
    NoExecute,
}

// --- Pod runtime info ---

/// Tracks which container runtime backend is running this pod.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PodRuntimeInfo {
    /// Backend name: "virtualization", "youki", "crun"
    pub backend: String,
    /// Version of the runtime
    pub version: String,
}

// --- Pod ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pod {
    pub id: String,
    pub name: String,
    pub namespace: String,
    pub spec: PodSpec,
    pub status: PodStatus,
    /// Human-readable reason for the current status (e.g. error message on failure).
    #[serde(default)]
    pub status_message: Option<String>,
    /// The OCI container ID for this pod (set by agent after container creation).
    #[serde(default)]
    pub container_id: Option<String>,
    /// The node this pod is assigned to (set by scheduler)
    #[serde(default)]
    pub node_name: Option<String>,
    /// Labels for selector-based matching
    #[serde(default)]
    pub labels: HashMap<String, String>,
    /// Owner reference (e.g. ReplicaSet ID that created this pod)
    #[serde(default)]
    pub owner_ref: Option<String>,
    /// Number of times this pod has been restarted
    #[serde(default)]
    pub restart_count: u32,
    /// Container runtime used for this pod
    #[serde(default)]
    pub runtime_info: Option<PodRuntimeInfo>,
    pub created_at: DateTime<Utc>,
}
