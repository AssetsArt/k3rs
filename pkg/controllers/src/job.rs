use chrono::Utc;
use pkg_scheduler::Scheduler;
use pkg_state::client::StateStore;
use pkg_types::job::{Job, JobCondition};
use pkg_types::pod::{Pod, PodStatus};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};
use uuid::Uuid;

/// Controller that manages Job lifecycle — creates Pods to completion.
pub struct JobController {
    store: StateStore,
    scheduler: Arc<Scheduler>,
    check_interval: Duration,
}

impl JobController {
    pub fn new(store: StateStore, scheduler: Arc<Scheduler>) -> Self {
        Self {
            store,
            scheduler,
            check_interval: Duration::from_secs(10),
        }
    }

    pub fn start(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            info!(
                "JobController started (interval={}s)",
                self.check_interval.as_secs()
            );
            let mut interval = tokio::time::interval(self.check_interval);
            loop {
                interval.tick().await;
                if let Err(e) = self.reconcile().await {
                    warn!("JobController reconcile error: {}", e);
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
        let job_prefix = format!("/registry/jobs/{}/", ns);
        let job_entries = self.store.list_prefix(&job_prefix).await?;

        let node_entries = self.store.list_prefix("/registry/nodes/").await?;
        let nodes: Vec<pkg_types::node::Node> = node_entries
            .into_iter()
            .filter_map(|(_, v)| serde_json::from_slice(&v).ok())
            .collect();

        for (job_key, job_value) in job_entries {
            let mut job: Job = match serde_json::from_slice(&job_value) {
                Ok(j) => j,
                Err(_) => continue,
            };

            // Skip completed or failed jobs
            if job.status.condition == JobCondition::Complete
                || job.status.condition == JobCondition::Failed
            {
                continue;
            }

            // Set start time if not set
            if job.status.start_time.is_none() {
                job.status.start_time = Some(Utc::now());
            }

            // Get owned pods
            let pod_prefix = format!("/registry/pods/{}/", ns);
            let pod_entries = self.store.list_prefix(&pod_prefix).await?;
            let owned_pods: Vec<(String, Pod)> = pod_entries
                .into_iter()
                .filter_map(|(k, v)| {
                    let pod: Pod = serde_json::from_slice(&v).ok()?;
                    if pod.owner_ref.as_deref() == Some(&job.id) {
                        Some((k, pod))
                    } else {
                        None
                    }
                })
                .collect();

            // Count pod states
            let active = owned_pods
                .iter()
                .filter(|(_, p)| {
                    matches!(
                        p.status,
                        PodStatus::Pending | PodStatus::Scheduled | PodStatus::Running
                    )
                })
                .count() as u32;
            let succeeded = owned_pods
                .iter()
                .filter(|(_, p)| p.status == PodStatus::Succeeded)
                .count() as u32;
            let failed = owned_pods
                .iter()
                .filter(|(_, p)| p.status == PodStatus::Failed)
                .count() as u32;

            job.status.active = active;
            job.status.succeeded = succeeded;
            job.status.failed = failed;

            // Check completion
            if succeeded >= job.spec.completions {
                job.status.condition = JobCondition::Complete;
                job.status.completion_time = Some(Utc::now());
                info!("Job {}: completed ({} succeeded)", job.name, succeeded);
            } else if failed >= job.spec.backoff_limit {
                job.status.condition = JobCondition::Failed;
                info!(
                    "Job {}: failed (backoff limit {} reached)",
                    job.name, job.spec.backoff_limit
                );
            } else if active < job.spec.parallelism && (active + succeeded) < job.spec.completions {
                // Need to create more pods
                let to_create =
                    (job.spec.parallelism - active).min(job.spec.completions - active - succeeded);
                for _ in 0..to_create {
                    self.create_job_pod(ns, &job, &nodes).await?;
                    info!("Job {}: created new pod", job.name);
                }
            }

            let data = serde_json::to_vec(&job)?;
            self.store.put(&job_key, &data).await?;
        }
        Ok(())
    }

    async fn create_job_pod(
        &self,
        ns: &str,
        job: &Job,
        nodes: &[pkg_types::node::Node],
    ) -> anyhow::Result<Pod> {
        let pod_id = Uuid::new_v4().to_string();
        let mut pod = Pod {
            id: pod_id.clone(),
            name: format!("{}-{}", job.name, &pod_id[..8]),
            namespace: ns.to_string(),
            spec: job.spec.template.clone(),
            status: PodStatus::Pending,
            node_name: None,
            labels: HashMap::new(),
            owner_ref: Some(job.id.clone()),
            restart_count: 0,
            runtime_info: None,
            created_at: Utc::now(),
        };

        if let Some(node_name) = self.scheduler.schedule(&pod, nodes) {
            pod.node_name = Some(node_name);
            pod.status = PodStatus::Scheduled;
        }

        let key = format!("/registry/pods/{}/{}", ns, pod_id);
        let data = serde_json::to_vec(&pod)?;
        self.store.put(&key, &data).await?;
        Ok(pod)
    }
}
