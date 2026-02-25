use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// --- Registration messages ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeRegistrationRequest {
    pub token: String,
    pub node_name: String,
    #[serde(default)]
    pub labels: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeRegistrationResponse {
    pub node_id: String,
    pub certificate: String,
    pub private_key: String,
    pub server_ca: String,
}

// --- Persisted Node object ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NodeStatus {
    Ready,
    NotReady,
    Unknown,
}

impl std::fmt::Display for NodeStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NodeStatus::Ready => write!(f, "Ready"),
            NodeStatus::NotReady => write!(f, "NotReady"),
            NodeStatus::Unknown => write!(f, "Unknown"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub name: String,
    pub status: NodeStatus,
    pub registered_at: DateTime<Utc>,
    pub labels: std::collections::HashMap<String, String>,
}

// --- Cluster info ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterInfo {
    pub endpoint: String,
    pub version: String,
    pub state_store: String,
    pub node_count: usize,
}
