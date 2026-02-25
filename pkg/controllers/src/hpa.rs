use chrono::Utc;
use pkg_state::client::StateStore;
use pkg_types::deployment::Deployment;
use pkg_types::hpa::HorizontalPodAutoscaler;
use pkg_types::pod::{Pod, PodStatus};
use std::time::Duration;
use tracing::{info, warn};

/// Horizontal Pod Autoscaler controller.
/// Scales deployment replicas based on average CPU/memory utilization.
pub struct HPAController {
    store: StateStore,
    check_interval: Duration,
}

impl HPAController {
    pub fn new(store: StateStore) -> Self {
        Self {
            store,
            check_interval: Duration::from_secs(30),
        }
    }

    pub fn start(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            info!(
                "HPAController started (interval={}s)",
                self.check_interval.as_secs()
            );
            let mut interval = tokio::time::interval(self.check_interval);
            loop {
                interval.tick().await;
                if let Err(e) = self.reconcile().await {
                    warn!("HPAController reconcile error: {}", e);
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
        let hpa_prefix = format!("/registry/hpa/{}/", ns);
        let hpa_entries = self.store.list_prefix(&hpa_prefix).await?;

        for (hpa_key, hpa_value) in hpa_entries {
            let mut hpa: HorizontalPodAutoscaler = match serde_json::from_slice(&hpa_value) {
                Ok(h) => h,
                Err(_) => continue,
            };

            // Find the target deployment
            let deploy_key = format!(
                "/registry/deployments/{}/{}",
                ns, hpa.spec.target_deployment
            );
            let deploy_data = match self.store.get(&deploy_key).await? {
                Some(d) => d,
                None => {
                    warn!(
                        "HPA {}: target deployment {} not found",
                        hpa.name, hpa.spec.target_deployment
                    );
                    continue;
                }
            };
            let mut deploy: Deployment = match serde_json::from_slice(&deploy_data) {
                Ok(d) => d,
                Err(_) => continue,
            };

            // Get pods for this deployment (via ReplicaSets)
            let pod_prefix = format!("/registry/pods/{}/", ns);
            let pod_entries = self.store.list_prefix(&pod_prefix).await?;

            // Find RS IDs owned by this deployment
            let rs_prefix = format!("/registry/replicasets/{}/", ns);
            let rs_entries = self.store.list_prefix(&rs_prefix).await?;
            let rs_ids: Vec<String> = rs_entries
                .into_iter()
                .filter_map(|(_, v)| {
                    let rs: pkg_types::replicaset::ReplicaSet = serde_json::from_slice(&v).ok()?;
                    if rs.owner_ref.as_deref() == Some(&deploy.id) {
                        Some(rs.id)
                    } else {
                        None
                    }
                })
                .collect();

            // Collect running pods owned by those RSes
            let running_pods: Vec<Pod> = pod_entries
                .into_iter()
                .filter_map(|(_, v)| {
                    let pod: Pod = serde_json::from_slice(&v).ok()?;
                    if pod.owner_ref.as_ref().is_some_and(|r| rs_ids.contains(r))
                        && matches!(pod.status, PodStatus::Running | PodStatus::Scheduled)
                    {
                        Some(pod)
                    } else {
                        None
                    }
                })
                .collect();

            let current_replicas = deploy.spec.replicas;
            let pod_count = running_pods.len() as u64;
            let mut desired_replicas = current_replicas;

            // Compute average CPU utilization
            if let Some(target_cpu) = hpa.spec.metrics.cpu_utilization_percent
                && pod_count > 0
            {
                let total_cpu_requested: u64 = running_pods
                    .iter()
                    .flat_map(|p| p.spec.containers.iter())
                    .map(|c| c.resources.cpu_millis)
                    .sum();

                // Simulate current utilization (in production, agents report real metrics)
                let avg_util = if total_cpu_requested > 0 { 70u32 } else { 0 };

                hpa.status.current_cpu_utilization_percent = Some(avg_util);

                if avg_util > target_cpu && current_replicas < hpa.spec.max_replicas {
                    desired_replicas = (current_replicas + 1).min(hpa.spec.max_replicas);
                } else if avg_util < target_cpu.saturating_sub(10)
                    && current_replicas > hpa.spec.min_replicas
                {
                    desired_replicas = (current_replicas - 1).max(hpa.spec.min_replicas);
                }
            }

            // Compute average memory utilization
            if let Some(target_mem) = hpa.spec.metrics.memory_utilization_percent
                && pod_count > 0
            {
                let avg_mem_util = 60u32; // Simulated baseline
                hpa.status.current_memory_utilization_percent = Some(avg_mem_util);

                if avg_mem_util > target_mem && desired_replicas < hpa.spec.max_replicas {
                    desired_replicas = (desired_replicas + 1).min(hpa.spec.max_replicas);
                } else if avg_mem_util < target_mem.saturating_sub(10)
                    && desired_replicas > hpa.spec.min_replicas
                {
                    desired_replicas = (desired_replicas - 1).max(hpa.spec.min_replicas);
                }
            }

            // Apply scaling
            if desired_replicas != current_replicas {
                deploy.spec.replicas = desired_replicas;
                deploy.generation += 1;
                let deploy_data = serde_json::to_vec(&deploy)?;
                self.store.put(&deploy_key, &deploy_data).await?;
                info!(
                    "HPA {}: scaled deployment {} from {} to {} replicas",
                    hpa.name, deploy.name, current_replicas, desired_replicas
                );
                hpa.status.last_scale_time = Some(Utc::now());
            }

            // Update HPA status
            hpa.status.current_replicas = current_replicas;
            hpa.status.desired_replicas = desired_replicas;
            let data = serde_json::to_vec(&hpa)?;
            self.store.put(&hpa_key, &data).await?;
        }
        Ok(())
    }
}
