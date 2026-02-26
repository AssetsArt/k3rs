use chrono::Utc;
use pkg_container::ContainerRuntime;
use pkg_scheduler::Scheduler;
use pkg_state::client::StateStore;
use pkg_types::pod::{Pod, PodRuntimeInfo, PodStatus};
use pkg_types::replicaset::ReplicaSet;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};
use uuid::Uuid;

/// Controller that reconciles ReplicaSets into Pods.
/// Ensures the desired number of pod replicas are running.
pub struct ReplicaSetController {
    store: StateStore,
    scheduler: Arc<Scheduler>,
    runtime: Arc<ContainerRuntime>,
    check_interval: Duration,
}

impl ReplicaSetController {
    pub fn new(
        store: StateStore,
        scheduler: Arc<Scheduler>,
        runtime: Arc<ContainerRuntime>,
    ) -> Self {
        Self {
            store,
            scheduler,
            runtime,
            check_interval: Duration::from_secs(10),
        }
    }

    pub fn start(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            info!(
                "ReplicaSetController started (interval={}s)",
                self.check_interval.as_secs()
            );
            let mut interval = tokio::time::interval(self.check_interval);
            loop {
                interval.tick().await;
                if let Err(e) = self.reconcile().await {
                    warn!("ReplicaSetController reconcile error: {}", e);
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
        let rs_prefix = format!("/registry/replicasets/{}/", ns);
        let rs_entries = self.store.list_prefix(&rs_prefix).await?;

        // Get all nodes for scheduling
        let node_entries = self.store.list_prefix("/registry/nodes/").await?;
        let nodes: Vec<pkg_types::node::Node> = node_entries
            .into_iter()
            .filter_map(|(_, v)| serde_json::from_slice(&v).ok())
            .collect();

        for (rs_key, rs_value) in rs_entries {
            let mut rs: ReplicaSet = match serde_json::from_slice(&rs_value) {
                Ok(r) => r,
                Err(_) => continue,
            };

            // Find pods owned by this RS
            let pod_prefix = format!("/registry/pods/{}/", ns);
            let pod_entries = self.store.list_prefix(&pod_prefix).await?;
            let owned_pods: Vec<(String, Pod)> = pod_entries
                .into_iter()
                .filter_map(|(k, v)| {
                    let pod: Pod = serde_json::from_slice(&v).ok()?;
                    if pod.owner_ref.as_deref() == Some(&rs.id) {
                        Some((k, pod))
                    } else {
                        None
                    }
                })
                .collect();

            let current_count = owned_pods.len() as u32;

            if current_count < rs.spec.replicas {
                // Scale up — create missing pods
                let to_create = rs.spec.replicas - current_count;
                for i in 0..to_create {
                    let pod = self.create_pod(ns, &rs, &nodes, i + current_count).await?;
                    info!(
                        "RS {}: created pod {} ({}/{})",
                        rs.name,
                        pod.name,
                        current_count + i + 1,
                        rs.spec.replicas
                    );
                }
            } else if current_count > rs.spec.replicas {
                // Scale down — delete excess pods (newest first)
                let to_delete = (current_count - rs.spec.replicas) as usize;
                let mut pods_to_delete: Vec<(String, Pod)> = owned_pods;
                pods_to_delete.sort_by_key(|b| std::cmp::Reverse(b.1.created_at));
                for (pod_key, pod) in pods_to_delete.into_iter().take(to_delete) {
                    self.store.delete(&pod_key).await?;
                    info!("RS {}: deleted pod {}", rs.name, pod.name);
                }
            }

            // Create real containers for Scheduled pods.
            // Pulls image, creates container, starts it, then marks Running.
            // If container creation fails, marks the pod as Failed.
            let pod_prefix_check = format!("/registry/pods/{}/", ns);
            let current_pods = self.store.list_prefix(&pod_prefix_check).await?;
            for (pod_key, pod_value) in current_pods {
                if let Ok(pod) = serde_json::from_slice::<Pod>(&pod_value)
                    && pod.owner_ref.as_deref() == Some(&rs.id)
                    && (pod.status == PodStatus::Scheduled
                        || pod.status == PodStatus::ContainerCreating)
                {
                    let container_id = &pod.id;

                    // Get image from first container spec
                    let image = pod
                        .spec
                        .containers
                        .first()
                        .map(|c| c.image.clone())
                        .unwrap_or_else(|| "busybox:latest".to_string());

                    let command: Vec<String> = pod
                        .spec
                        .containers
                        .first()
                        .map(|c| c.command.clone())
                        .unwrap_or_default();

                    // Immediately transition to ContainerCreating so the UI
                    // reflects the real state while we pull images / create the VM.
                    if pod.status == PodStatus::Scheduled {
                        let mut creating = pod.clone();
                        creating.status = PodStatus::ContainerCreating;
                        let data = serde_json::to_vec(&creating)?;
                        self.store.put(&pod_key, &data).await?;
                    }

                    // Actually create and start the container (may take minutes)
                    let mut updated = pod.clone();
                    match self
                        .create_real_container(container_id, &image, &command)
                        .await
                    {
                        Ok(()) => {
                            updated.status = PodStatus::Running;
                            updated.runtime_info = Some(PodRuntimeInfo {
                                backend: self.runtime.backend_name().to_string(),
                                version: self.runtime.runtime_info().version,
                            });
                            info!(
                                "RS {}: container created for pod {} via {} → Running",
                                rs.name,
                                pod.name,
                                self.runtime.backend_name()
                            );
                        }
                        Err(e) => {
                            warn!(
                                "RS {}: failed to create container for pod {}: {}",
                                rs.name, pod.name, e
                            );
                            updated.status = PodStatus::Failed;
                        }
                    }

                    let data = serde_json::to_vec(&updated)?;
                    self.store.put(&pod_key, &data).await?;
                }
            }

            // Recount pods for status update
            let pod_prefix = format!("/registry/pods/{}/", ns);
            let pod_entries = self.store.list_prefix(&pod_prefix).await?;
            let mut replicas = 0u32;
            let mut ready = 0u32;
            let mut available = 0u32;
            for (_, v) in pod_entries {
                if let Ok(pod) = serde_json::from_slice::<Pod>(&v) {
                    if pod.owner_ref.as_deref() != Some(&rs.id) {
                        continue;
                    }
                    replicas += 1;
                    match pod.status {
                        PodStatus::Running => {
                            ready += 1;
                            available += 1;
                        }
                        PodStatus::Scheduled => {
                            ready += 1;
                        }
                        _ => {}
                    }
                }
            }

            rs.status.replicas = replicas;
            rs.status.ready_replicas = ready;
            rs.status.available_replicas = available;
            let data = serde_json::to_vec(&rs)?;
            self.store.put(&rs_key, &data).await?;
        }
        Ok(())
    }

    async fn create_pod(
        &self,
        ns: &str,
        rs: &ReplicaSet,
        nodes: &[pkg_types::node::Node],
        _index: u32,
    ) -> anyhow::Result<Pod> {
        let pod_id = Uuid::new_v4().to_string();
        let mut pod = Pod {
            id: pod_id.clone(),
            name: format!("{}-{}", rs.name, &pod_id[..8]),
            namespace: ns.to_string(),
            spec: rs.spec.template.clone(),
            status: PodStatus::Pending,
            node_name: None,
            labels: rs.spec.selector.clone(),
            owner_ref: Some(rs.id.clone()),
            restart_count: 0,
            runtime_info: None,
            created_at: Utc::now(),
        };

        // Schedule the pod
        if let Some(node_name) = self.scheduler.schedule(&pod, nodes) {
            pod.node_name = Some(node_name);
            pod.status = PodStatus::Scheduled;
        }

        let key = format!("/registry/pods/{}/{}", ns, pod_id);
        let data = serde_json::to_vec(&pod)?;
        self.store.put(&key, &data).await?;
        Ok(pod)
    }

    /// Actually create and start a container via the runtime backend.
    async fn create_real_container(
        &self,
        container_id: &str,
        image: &str,
        command: &[String],
    ) -> anyhow::Result<()> {
        // Pull image (skipped for Docker backend which handles it internally)
        self.runtime.pull_image(image).await?;

        // Create container
        self.runtime
            .create_container(container_id, image, command)
            .await?;

        // Start container
        self.runtime.start_container(container_id).await?;

        Ok(())
    }
}
