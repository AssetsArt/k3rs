use pkg_state::client::StateStore;
use pkg_types::node::{Node, NodeStatus};
use pkg_types::pod::{Pod, PodStatus};
use std::time::Duration;
use tracing::{info, warn};

/// Controller that watches for failed nodes and reschedules their pods.
///
/// When a node stays in `Unknown` state past the eviction grace period,
/// all pods on that node are reset to `Pending` for rescheduling.
pub struct EvictionController {
    store: StateStore,
    check_interval: Duration,
    grace_period: Duration,
}

impl EvictionController {
    pub fn new(store: StateStore) -> Self {
        Self {
            store,
            check_interval: Duration::from_secs(30),
            grace_period: Duration::from_secs(300), // 5 minutes
        }
    }

    /// Start the controller loop as a background task.
    pub fn start(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            info!(
                "EvictionController started (interval={}s, grace={}s)",
                self.check_interval.as_secs(),
                self.grace_period.as_secs()
            );
            let mut interval = tokio::time::interval(self.check_interval);
            loop {
                interval.tick().await;
                if let Err(e) = self.reconcile().await {
                    warn!("EvictionController reconcile error: {}", e);
                }
            }
        })
    }

    /// One pass: find nodes in Unknown state past grace period and evict their pods.
    async fn reconcile(&self) -> anyhow::Result<()> {
        let node_entries = self.store.list_prefix("/registry/nodes/").await?;
        let now = chrono::Utc::now();

        let mut failed_node_ids: Vec<String> = Vec::new();

        for (_key, value) in &node_entries {
            let node: Node = match serde_json::from_slice(value) {
                Ok(n) => n,
                Err(_) => continue,
            };

            // Skip master nodes
            if node.labels.contains_key("node-role.kubernetes.io/master")
                || node
                    .labels
                    .contains_key("node-role.kubernetes.io/control-plane")
            {
                continue;
            }

            if node.status == NodeStatus::Unknown {
                let age = now
                    .signed_duration_since(node.last_heartbeat)
                    .to_std()
                    .unwrap_or_default();

                if age >= self.grace_period {
                    info!(
                        "Node {} has been Unknown for {}s (grace={}s) — evicting pods",
                        node.name,
                        age.as_secs(),
                        self.grace_period.as_secs()
                    );
                    failed_node_ids.push(node.name.clone());
                }
            }
        }

        if failed_node_ids.is_empty() {
            return Ok(());
        }

        // Evict pods from failed nodes
        let pod_entries = self.store.list_prefix("/registry/pods/").await?;
        for (key, value) in pod_entries {
            let mut pod: Pod = match serde_json::from_slice(&value) {
                Ok(p) => p,
                Err(_) => continue,
            };

            if let Some(ref node_id) = pod.node_id
                && failed_node_ids.contains(node_id)
                && pod.status != PodStatus::Pending
                && pod.status != PodStatus::Succeeded
                && pod.status != PodStatus::Failed
            {
                info!("Evicting pod {} (was on failed node {})", pod.name, node_id);
                pod.node_id = None;
                pod.status = PodStatus::Pending;
                let data = serde_json::to_vec(&pod)?;
                self.store.put(&key, &data).await?;
            }
        }

        Ok(())
    }
}
