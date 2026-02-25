use chrono::Utc;
use pkg_state::client::StateStore;
use pkg_types::node::{Node, NodeStatus};
use std::time::Duration;
use tracing::{info, warn};

/// Background controller that monitors node health based on heartbeat timestamps.
/// Transitions nodes: Ready → NotReady (30s stale) → Unknown (60s stale).
pub struct NodeController {
    store: StateStore,
    check_interval: Duration,
    not_ready_threshold: Duration,
    unknown_threshold: Duration,
}

impl NodeController {
    pub fn new(store: StateStore) -> Self {
        Self {
            store,
            check_interval: Duration::from_secs(15),
            not_ready_threshold: Duration::from_secs(30),
            unknown_threshold: Duration::from_secs(60),
        }
    }

    /// Start the controller loop as a background task.
    pub fn start(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            info!(
                "NodeController started (interval={}s)",
                self.check_interval.as_secs()
            );
            let mut interval = tokio::time::interval(self.check_interval);
            loop {
                interval.tick().await;
                if let Err(e) = self.reconcile().await {
                    warn!("NodeController reconcile error: {}", e);
                }
            }
        })
    }

    /// One pass: check all nodes and update stale ones.
    async fn reconcile(&self) -> anyhow::Result<()> {
        let entries = self.store.list_prefix("/registry/nodes/").await?;
        let now = Utc::now();

        for (key, value) in entries {
            let mut node: Node = match serde_json::from_slice(&value) {
                Ok(n) => n,
                Err(_) => continue,
            };

            let age = now
                .signed_duration_since(node.last_heartbeat)
                .to_std()
                .unwrap_or_default();

            let is_master = node.labels.contains_key("node-role.kubernetes.io/master")
                || node
                    .labels
                    .contains_key("node-role.kubernetes.io/control-plane");

            let new_status = if is_master {
                // The master node runs alongside the server and doesn't send heartbeats.
                NodeStatus::Ready
            } else if age >= self.unknown_threshold {
                NodeStatus::Unknown
            } else if age >= self.not_ready_threshold {
                NodeStatus::NotReady
            } else {
                NodeStatus::Ready
            };

            if node.status != new_status {
                info!(
                    "Node {} status: {} → {} (last heartbeat {}s ago)",
                    node.name,
                    node.status,
                    new_status,
                    age.as_secs()
                );
                node.status = new_status;
                let data = serde_json::to_vec(&node)?;
                self.store.put(&key, &data).await?;
            }
        }
        Ok(())
    }
}
