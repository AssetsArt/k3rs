use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::pod::PodSpec;

// --- ReplicaSet status ---

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReplicaSetStatus {
    pub replicas: u32,
    pub ready_replicas: u32,
    pub available_replicas: u32,
}

// --- ReplicaSet spec ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplicaSetSpec {
    pub replicas: u32,
    #[serde(default)]
    pub selector: HashMap<String, String>,
    pub template: PodSpec,
}

// --- ReplicaSet ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplicaSet {
    pub id: String,
    pub name: String,
    pub namespace: String,
    pub spec: ReplicaSetSpec,
    #[serde(default)]
    pub status: ReplicaSetStatus,
    /// Owner reference (Deployment ID that manages this RS)
    #[serde(default)]
    pub owner_ref: Option<String>,
    /// Template hash for tracking which spec version this RS represents
    #[serde(default)]
    pub template_hash: String,
    pub created_at: DateTime<Utc>,
}
