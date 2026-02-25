use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Path matching type for Ingress rules.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum PathType {
    #[default]
    Prefix,
    Exact,
}

/// Backend service target for an Ingress path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngressBackend {
    pub service_name: String,
    pub service_port: u16,
}

/// A single path rule within an Ingress HTTP rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngressPath {
    pub path: String,
    #[serde(default)]
    pub path_type: PathType,
    pub backend: IngressBackend,
}

/// HTTP rules for a host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngressHTTP {
    pub paths: Vec<IngressPath>,
}

/// A single host-based Ingress rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngressRule {
    pub host: String,
    pub http: IngressHTTP,
}

/// TLS configuration for an Ingress.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngressTLS {
    pub hosts: Vec<String>,
    pub secret_name: String,
}

/// Ingress specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngressSpec {
    pub rules: Vec<IngressRule>,
    #[serde(default)]
    pub tls: Option<Vec<IngressTLS>>,
}

/// Ingress resource for external traffic routing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ingress {
    pub id: String,
    pub name: String,
    pub namespace: String,
    pub spec: IngressSpec,
    pub created_at: DateTime<Utc>,
}
