use chrono::Utc;
use pkg_state::client::StateStore;
use pkg_types::job::{CronJob, Job, JobStatus};
use std::time::Duration;
use tracing::{info, warn};
use uuid::Uuid;

/// Controller that creates Jobs on a cron schedule.
/// Supports simple cron expressions: `*/N` for every N minutes and exact minute values.
pub struct CronJobController {
    store: StateStore,
    check_interval: Duration,
}

impl CronJobController {
    pub fn new(store: StateStore) -> Self {
        Self {
            store,
            check_interval: Duration::from_secs(30),
        }
    }

    pub fn start(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            info!(
                "CronJobController started (interval={}s)",
                self.check_interval.as_secs()
            );
            let mut interval = tokio::time::interval(self.check_interval);
            loop {
                interval.tick().await;
                if let Err(e) = self.reconcile().await {
                    warn!("CronJobController reconcile error: {}", e);
                }
            }
        })
    }

    async fn reconcile(&self) -> anyhow::Result<()> {
        let ns_entries = self.store.list_prefix("/registry/namespaces/").await?;
        for (ns_key, _) in ns_entries {
            let ns = ns_key
                .strip_prefix("/registry/namespaces/")
                .unwrap_or_default()
                .to_string();
            if ns.is_empty() {
                continue;
            }
            self.reconcile_namespace(&ns).await?;
        }
        Ok(())
    }

    async fn reconcile_namespace(&self, ns: &str) -> anyhow::Result<()> {
        let cj_prefix = format!("/registry/cronjobs/{}/", ns);
        let cj_entries = self.store.list_prefix(&cj_prefix).await?;
        let now = Utc::now();

        for (cj_key, cj_value) in cj_entries {
            let mut cj: CronJob = match serde_json::from_slice(&cj_value) {
                Ok(c) => c,
                Err(_) => continue,
            };

            // Skip suspended CronJobs
            if cj.spec.suspend {
                continue;
            }

            // Check if schedule is due
            let should_run = match &cj.status.last_schedule_time {
                Some(last) => is_schedule_due(&cj.spec.schedule, last, &now),
                None => true, // Never ran before
            };

            if should_run {
                // Create a new Job
                let job_id = Uuid::new_v4().to_string();
                let job = Job {
                    id: job_id.clone(),
                    name: format!("{}-{}", cj.name, now.format("%Y%m%d%H%M%S")),
                    namespace: ns.to_string(),
                    spec: cj.spec.job_template.clone(),
                    status: JobStatus::default(),
                    owner_ref: Some(cj.id.clone()),
                    created_at: now,
                };

                let job_key = format!("/registry/jobs/{}/{}", ns, job_id);
                let data = serde_json::to_vec(&job)?;
                self.store.put(&job_key, &data).await?;

                info!(
                    "CronJob {}: spawned job {} (schedule={})",
                    cj.name, job.name, cj.spec.schedule
                );

                // Update CronJob status
                cj.status.last_schedule_time = Some(now);
                cj.status.active_jobs.push(job_id);

                let data = serde_json::to_vec(&cj)?;
                self.store.put(&cj_key, &data).await?;
            }

            // Clean up completed jobs from active list
            let mut still_active = Vec::new();
            for job_id in &cj.status.active_jobs {
                let job_key = format!("/registry/jobs/{}/{}", ns, job_id);
                let is_running = self
                    .store
                    .get(&job_key)
                    .await
                    .ok()
                    .flatten()
                    .and_then(|data| serde_json::from_slice::<Job>(&data).ok())
                    .is_some_and(|job| {
                        job.status.condition == pkg_types::job::JobCondition::Running
                    });
                if is_running {
                    still_active.push(job_id.clone());
                }
            }
            if still_active.len() != cj.status.active_jobs.len() {
                cj.status.active_jobs = still_active;
                let data = serde_json::to_vec(&cj)?;
                self.store.put(&cj_key, &data).await?;
            }
        }
        Ok(())
    }
}

/// Simple cron schedule checker.
/// Supports: `*/N * * * *` (every N minutes) and `M * * * *` (at minute M).
/// Only parses the minute field for MVP.
fn is_schedule_due(
    schedule: &str,
    last_run: &chrono::DateTime<chrono::Utc>,
    now: &chrono::DateTime<chrono::Utc>,
) -> bool {
    let parts: Vec<&str> = schedule.split_whitespace().collect();
    if parts.is_empty() {
        return false;
    }

    let minute_field = parts[0];
    let elapsed_minutes = now.signed_duration_since(*last_run).num_minutes();

    if minute_field == "*" {
        // Every minute
        elapsed_minutes >= 1
    } else if let Some(interval) = minute_field.strip_prefix("*/") {
        // Every N minutes
        if let Ok(n) = interval.parse::<i64>() {
            elapsed_minutes >= n
        } else {
            false
        }
    } else if let Ok(target_minute) = minute_field.parse::<u32>() {
        // At specific minute — check if we crossed that minute boundary
        let current_minute = now.format("%M").to_string().parse::<u32>().unwrap_or(0);
        current_minute == target_minute && elapsed_minutes >= 1
    } else {
        false
    }
}
