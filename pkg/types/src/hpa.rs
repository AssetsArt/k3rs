use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// --- HPA metric targets ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricTarget {
    /// Target average CPU utilization (percentage, e.g. 80 = 80%)
    #[serde(default)]
    pub cpu_utilization_percent: Option<u32>,
    /// Target average memory utilization (percentage)
    #[serde(default)]
    pub memory_utilization_percent: Option<u32>,
}

// --- HPA status ---

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HPAStatus {
    pub current_replicas: u32,
    pub desired_replicas: u32,
    #[serde(default)]
    pub current_cpu_utilization_percent: Option<u32>,
    #[serde(default)]
    pub current_memory_utilization_percent: Option<u32>,
    #[serde(default)]
    pub last_scale_time: Option<DateTime<Utc>>,
}

// --- HPA spec ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HPASpec {
    /// Target deployment ID to scale
    pub target_deployment: String,
    pub min_replicas: u32,
    pub max_replicas: u32,
    pub metrics: MetricTarget,
}

// --- HPA ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HorizontalPodAutoscaler {
    pub id: String,
    pub name: String,
    pub namespace: String,
    pub spec: HPASpec,
    #[serde(default)]
    pub status: HPAStatus,
    pub created_at: DateTime<Utc>,
}
