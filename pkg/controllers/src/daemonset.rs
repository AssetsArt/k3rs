use chrono::Utc;
use pkg_state::client::StateStore;
use pkg_types::daemonset::DaemonSet;
use pkg_types::node::{Node, NodeStatus};
use pkg_types::pod::{Pod, PodStatus};
use std::collections::HashMap;
use std::time::Duration;
use tracing::{info, warn};
use uuid::Uuid;

/// Controller that ensures exactly one Pod runs on each eligible node
/// for every DaemonSet.
pub struct DaemonSetController {
    store: StateStore,
    check_interval: Duration,
}

impl DaemonSetController {
    pub fn new(store: StateStore) -> Self {
        Self {
            store,
            check_interval: Duration::from_secs(15),
        }
    }

    pub fn start(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            info!(
                "DaemonSetController started (interval={}s)",
                self.check_interval.as_secs()
            );
            let mut interval = tokio::time::interval(self.check_interval);
            loop {
                interval.tick().await;
                if let Err(e) = self.reconcile().await {
                    warn!("DaemonSetController reconcile error: {}", e);
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
        // Get all nodes
        let node_entries = self.store.list_prefix("/registry/nodes/").await?;
        let nodes: Vec<Node> = node_entries
            .into_iter()
            .filter_map(|(_, v)| serde_json::from_slice(&v).ok())
            .collect();

        let ds_prefix = format!("/registry/daemonsets/{}/", ns);
        let ds_entries = self.store.list_prefix(&ds_prefix).await?;

        for (ds_key, ds_value) in ds_entries {
            let mut ds: DaemonSet = match serde_json::from_slice(&ds_value) {
                Ok(d) => d,
                Err(_) => continue,
            };

            // Filter eligible nodes (Ready + matching node_selector)
            let eligible_nodes: Vec<&Node> = nodes
                .iter()
                .filter(|n| {
                    if n.status != NodeStatus::Ready {
                        return false;
                    }
                    // Check node selector
                    ds.spec
                        .node_selector
                        .iter()
                        .all(|(k, v)| n.labels.get(k).is_some_and(|nv| nv == v))
                })
                .collect();

            // Get pods owned by this DaemonSet
            let pod_prefix = format!("/registry/pods/{}/", ns);
            let pod_entries = self.store.list_prefix(&pod_prefix).await?;
            let owned_pods: Vec<(String, Pod)> = pod_entries
                .into_iter()
                .filter_map(|(k, v)| {
                    let pod: Pod = serde_json::from_slice(&v).ok()?;
                    if pod.owner_ref.as_deref() == Some(&ds.id) {
                        Some((k, pod))
                    } else {
                        None
                    }
                })
                .collect();

            // Build set of node IDs that already have a pod
            let nodes_with_pods: HashMap<String, String> = owned_pods
                .iter()
                .filter_map(|(key, pod)| {
                    pod.node_name.as_ref().map(|nid| (nid.clone(), key.clone()))
                })
                .collect();

            // Create pods on nodes that don't have one
            let mut created = 0u32;
            for node in &eligible_nodes {
                if !nodes_with_pods.contains_key(&node.id) {
                    self.create_pod_on_node(ns, &ds, node).await?;
                    created += 1;
                    info!("DaemonSet {}: created pod on node {}", ds.name, node.name);
                }
            }

            // Delete pods on nodes that are no longer eligible
            let eligible_ids: std::collections::HashSet<&str> =
                eligible_nodes.iter().map(|n| n.id.as_str()).collect();
            for (pod_key, pod) in &owned_pods {
                if let Some(nid) = &pod.node_name
                    && !eligible_ids.contains(nid.as_str())
                {
                    self.store.delete(pod_key).await?;
                    info!(
                        "DaemonSet {}: removed orphan pod {} from node {}",
                        ds.name, pod.name, nid
                    );
                }
            }

            // Update status
            ds.status.desired_number_scheduled = eligible_nodes.len() as u32;
            ds.status.current_number_scheduled = eligible_nodes.len() as u32;
            let ready_count = owned_pods
                .iter()
                .filter(|(_, p)| {
                    matches!(p.status, PodStatus::Running | PodStatus::Scheduled)
                        && p.node_name
                            .as_ref()
                            .is_some_and(|nid| eligible_ids.contains(nid.as_str()))
                })
                .count() as u32
                + created;
            ds.status.number_ready = ready_count.min(ds.status.desired_number_scheduled);
            let data = serde_json::to_vec(&ds)?;
            self.store.put(&ds_key, &data).await?;
        }
        Ok(())
    }

    async fn create_pod_on_node(
        &self,
        ns: &str,
        ds: &DaemonSet,
        node: &Node,
    ) -> anyhow::Result<Pod> {
        let pod_id = Uuid::new_v4().to_string();
        let pod = Pod {
            id: pod_id.clone(),
            name: format!("{}-{}", ds.name, &node.name),
            namespace: ns.to_string(),
            spec: ds.spec.template.clone(),
            status: PodStatus::Scheduled,
            node_name: Some(node.name.clone()),
            labels: ds.spec.node_selector.clone(),
            owner_ref: Some(ds.id.clone()),
            restart_count: 0,
            runtime_info: None,
            created_at: Utc::now(),
        };
        let key = format!("/registry/pods/{}/{}", ns, pod_id);
        let data = serde_json::to_vec(&pod)?;
        self.store.put(&key, &data).await?;
        Ok(pod)
    }
}
