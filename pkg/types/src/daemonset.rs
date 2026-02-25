use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::pod::PodSpec;

// --- DaemonSet status ---

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DaemonSetStatus {
    pub desired_number_scheduled: u32,
    pub current_number_scheduled: u32,
    pub number_ready: u32,
}

// --- DaemonSet spec ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonSetSpec {
    pub template: PodSpec,
    /// Only schedule on nodes matching these labels
    #[serde(default)]
    pub node_selector: HashMap<String, String>,
}

// --- DaemonSet ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonSet {
    pub id: String,
    pub name: String,
    pub namespace: String,
    pub spec: DaemonSetSpec,
    #[serde(default)]
    pub status: DaemonSetStatus,
    pub created_at: DateTime<Utc>,
}
