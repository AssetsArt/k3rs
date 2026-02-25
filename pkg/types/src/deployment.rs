use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::pod::PodSpec;

// --- Deployment strategy ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DeploymentStrategy {
    RollingUpdate {
        #[serde(default = "default_max_surge")]
        max_surge: u32,
        #[serde(default = "default_max_unavailable")]
        max_unavailable: u32,
    },
    Recreate,
}

fn default_max_surge() -> u32 {
    1
}
fn default_max_unavailable() -> u32 {
    0
}

impl Default for DeploymentStrategy {
    fn default() -> Self {
        DeploymentStrategy::RollingUpdate {
            max_surge: 1,
            max_unavailable: 0,
        }
    }
}

// --- Deployment status ---

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeploymentStatus {
    pub ready_replicas: u32,
    pub available_replicas: u32,
    pub updated_replicas: u32,
}

// --- Deployment spec ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentSpec {
    pub replicas: u32,
    pub template: PodSpec,
    #[serde(default)]
    pub strategy: DeploymentStrategy,
}

// --- Deployment ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Deployment {
    pub id: String,
    pub name: String,
    pub namespace: String,
    pub spec: DeploymentSpec,
    #[serde(default)]
    pub status: DeploymentStatus,
    pub created_at: DateTime<Utc>,
}
