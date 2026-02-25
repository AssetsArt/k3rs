use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::pod::PodSpec;

// --- Job status ---

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum JobCondition {
    #[default]
    Running,
    Complete,
    Failed,
}

impl std::fmt::Display for JobCondition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JobCondition::Running => write!(f, "Running"),
            JobCondition::Complete => write!(f, "Complete"),
            JobCondition::Failed => write!(f, "Failed"),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JobStatus {
    pub active: u32,
    pub succeeded: u32,
    pub failed: u32,
    pub condition: JobCondition,
    #[serde(default)]
    pub start_time: Option<DateTime<Utc>>,
    #[serde(default)]
    pub completion_time: Option<DateTime<Utc>>,
}

// --- Job spec ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobSpec {
    pub template: PodSpec,
    /// Number of successful completions required
    #[serde(default = "default_completions")]
    pub completions: u32,
    /// Max pods running in parallel
    #[serde(default = "default_parallelism")]
    pub parallelism: u32,
    /// Max failures before marking job as Failed
    #[serde(default = "default_backoff_limit")]
    pub backoff_limit: u32,
}

fn default_completions() -> u32 {
    1
}
fn default_parallelism() -> u32 {
    1
}
fn default_backoff_limit() -> u32 {
    6
}

// --- Job ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub name: String,
    pub namespace: String,
    pub spec: JobSpec,
    #[serde(default)]
    pub status: JobStatus,
    /// Owner reference (CronJob ID if created by one)
    #[serde(default)]
    pub owner_ref: Option<String>,
    pub created_at: DateTime<Utc>,
}

// --- CronJob ---

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CronJobStatus {
    #[serde(default)]
    pub last_schedule_time: Option<DateTime<Utc>>,
    #[serde(default)]
    pub active_jobs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJobSpec {
    /// Cron schedule string (e.g. "*/5 * * * *")
    pub schedule: String,
    /// Job template to create
    pub job_template: JobSpec,
    /// If true, skip execution
    #[serde(default)]
    pub suspend: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJob {
    pub id: String,
    pub name: String,
    pub namespace: String,
    pub spec: CronJobSpec,
    #[serde(default)]
    pub status: CronJobStatus,
    pub created_at: DateTime<Utc>,
}
