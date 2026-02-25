use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Resource quota for a namespace — limits pod count, CPU, and memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceQuota {
    pub name: String,
    pub namespace: String,
    pub hard: QuotaLimits,
    #[serde(default)]
    pub used: QuotaUsage,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaLimits {
    /// Maximum number of pods allowed
    #[serde(default)]
    pub max_pods: Option<u32>,
    /// Maximum total CPU in millicores
    #[serde(default)]
    pub max_cpu_millis: Option<u64>,
    /// Maximum total memory in bytes
    #[serde(default)]
    pub max_memory_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct QuotaUsage {
    pub pods: u32,
    pub cpu_millis: u64,
    pub memory_bytes: u64,
}
