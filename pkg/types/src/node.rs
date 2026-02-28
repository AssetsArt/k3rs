use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::pod::ResourceRequirements;

// --- Registration messages ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeRegistrationRequest {
    pub token: String,
    pub node_name: String,
    /// The address (IP or hostname) where the agent's API is listening.
    pub address: String,
    #[serde(default)]
    pub labels: HashMap<String, String>,
    #[serde(default)]
    pub capacity: Option<ResourceRequirements>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeRegistrationResponse {
    /// UUID assigned to this node
    pub node_id: String,
    pub certificate: String,
    pub private_key: String,
    pub server_ca: String,
    /// Port for the agent to listen on for its API (assigned by server)
    pub agent_api_port: u16,
}

// --- Node status ---

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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

// --- Taint ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Taint {
    pub key: String,
    pub value: String,
    pub effect: crate::pod::TaintEffect,
}

// --- Persisted Node object ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub name: String,
    /// Agent API address (host:port)
    pub address: String,
    /// Port for the agent to listen on for its API
    pub agent_api_port: u16,
    pub status: NodeStatus,
    pub registered_at: DateTime<Utc>,
    pub last_heartbeat: DateTime<Utc>,
    pub labels: HashMap<String, String>,
    #[serde(default)]
    pub taints: Vec<Taint>,
    #[serde(default)]
    pub capacity: ResourceRequirements,
    #[serde(default)]
    pub allocated: ResourceRequirements,
    /// If true, the scheduler will not place new pods on this node.
    #[serde(default)]
    pub unschedulable: bool,
}

// --- Cluster info ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterInfo {
    pub endpoint: String,
    pub version: String,
    pub state_store: String,
    pub node_count: usize,
}
