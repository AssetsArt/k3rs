use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// An address of a backend pod serving a Service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndpointAddress {
    pub ip: String,
    #[serde(default)]
    pub node_id: Option<String>,
    #[serde(default)]
    pub pod_id: Option<String>,
}

/// A port exposed by a backend pod.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndpointPort {
    pub name: String,
    pub port: u16,
    #[serde(default = "default_protocol")]
    pub protocol: String,
}

fn default_protocol() -> String {
    "TCP".to_string()
}

/// Endpoint represents the set of backend addresses for a Service.
/// Equivalent to an EndpointSlice in Kubernetes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Endpoint {
    pub id: String,
    pub service_id: String,
    pub service_name: String,
    pub namespace: String,
    pub addresses: Vec<EndpointAddress>,
    pub ports: Vec<EndpointPort>,
    pub created_at: DateTime<Utc>,
}
