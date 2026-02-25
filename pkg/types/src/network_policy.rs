use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Network policy controlling ingress/egress traffic for pods.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkPolicy {
    pub name: String,
    pub namespace: String,
    /// Label selector for pods this policy applies to
    #[serde(default)]
    pub pod_selector: HashMap<String, String>,
    /// Which traffic directions this policy controls
    #[serde(default)]
    pub policy_types: Vec<PolicyType>,
    /// Allowed inbound traffic rules
    #[serde(default)]
    pub ingress: Vec<IngressRule>,
    /// Allowed outbound traffic rules
    #[serde(default)]
    pub egress: Vec<EgressRule>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PolicyType {
    Ingress,
    Egress,
}

/// Inbound traffic rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngressRule {
    /// Source peers allowed
    #[serde(default)]
    pub from: Vec<NetworkPolicyPeer>,
    /// Ports allowed
    #[serde(default)]
    pub ports: Vec<NetworkPolicyPort>,
}

/// Outbound traffic rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EgressRule {
    /// Destination peers allowed
    #[serde(default)]
    pub to: Vec<NetworkPolicyPeer>,
    /// Ports allowed
    #[serde(default)]
    pub ports: Vec<NetworkPolicyPort>,
}

/// A peer in a network policy (pod selector or namespace selector).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkPolicyPeer {
    /// Match pods with these labels
    #[serde(default)]
    pub pod_selector: Option<HashMap<String, String>>,
    /// Match namespaces with these labels
    #[serde(default)]
    pub namespace_selector: Option<HashMap<String, String>>,
}

/// A port in a network policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkPolicyPort {
    pub protocol: Option<String>,
    pub port: Option<u16>,
}
