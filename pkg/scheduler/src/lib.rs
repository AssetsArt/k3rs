use pkg_types::node::{Node, NodeStatus};
use pkg_types::pod::{Pod, TaintEffect, TolerationOperator};
use std::sync::atomic::{AtomicUsize, Ordering};
use tracing::info;

/// Round-robin scheduler with filtering for taints, tolerations,
/// node affinity, and resource availability.
pub struct Scheduler {
    round_robin_index: AtomicUsize,
}

impl Scheduler {
    pub fn new() -> Self {
        Self {
            round_robin_index: AtomicUsize::new(0),
        }
    }

    /// Schedule a pod to a node. Returns the node ID if a suitable node is found.
    pub fn schedule(&self, pod: &Pod, nodes: &[Node]) -> Option<String> {
        let eligible: Vec<&Node> = nodes
            .iter()
            .filter(|n| self.is_node_eligible(n, pod))
            .collect();

        if eligible.is_empty() {
            info!("No eligible nodes for pod {}/{}", pod.namespace, pod.name);
            return None;
        }

        // Round-robin selection among eligible nodes
        let idx = self.round_robin_index.fetch_add(1, Ordering::Relaxed) % eligible.len();
        let selected = eligible[idx];

        info!(
            "Scheduled pod {}/{} → node {} ({})",
            pod.namespace, pod.name, selected.name, selected.id
        );
        Some(selected.id.clone())
    }

    /// Check if a node is eligible to run this pod.
    fn is_node_eligible(&self, node: &Node, pod: &Pod) -> bool {
        // 1. Node must be Ready
        if node.status != NodeStatus::Ready {
            return false;
        }

        // 2. Node must not be unschedulable (cordoned)
        if node.unschedulable {
            return false;
        }

        // 2. Check node affinity (all required labels must match)
        for (key, value) in &pod.spec.node_affinity {
            match node.labels.get(key) {
                Some(v) if v == value => {}
                _ => return false,
            }
        }

        // 3. Check taints & tolerations
        for taint in &node.taints {
            let tolerated = pod.spec.tolerations.iter().any(|t| {
                if t.key != taint.key {
                    return false;
                }
                match t.operator {
                    TolerationOperator::Exists => true,
                    TolerationOperator::Equal => t.value == taint.value,
                }
            });
            // If a NoSchedule taint is not tolerated, skip this node
            if !tolerated {
                match taint.effect {
                    TaintEffect::NoSchedule | TaintEffect::NoExecute => return false,
                    TaintEffect::PreferNoSchedule => {} // soft preference, don't reject
                }
            }
        }

        // 4. Check resource availability
        let pod_cpu: u64 = pod
            .spec
            .containers
            .iter()
            .map(|c| c.resources.cpu_millis)
            .sum();
        let pod_mem: u64 = pod
            .spec
            .containers
            .iter()
            .map(|c| c.resources.memory_bytes)
            .sum();

        if node.capacity.cpu_millis > 0 {
            let available_cpu = node
                .capacity
                .cpu_millis
                .saturating_sub(node.allocated.cpu_millis);
            if pod_cpu > available_cpu {
                return false;
            }
        }
        if node.capacity.memory_bytes > 0 {
            let available_mem = node
                .capacity
                .memory_bytes
                .saturating_sub(node.allocated.memory_bytes);
            if pod_mem > available_mem {
                return false;
            }
        }

        true
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use pkg_types::pod::{ContainerSpec, PodSpec, PodStatus, ResourceRequirements};
    use std::collections::HashMap;

    fn make_node(name: &str, status: NodeStatus) -> Node {
        Node {
            id: format!("{}-id", name),
            name: name.to_string(),
            status,
            registered_at: Utc::now(),
            last_heartbeat: Utc::now(),
            labels: HashMap::new(),
            taints: vec![],
            capacity: ResourceRequirements {
                cpu_millis: 4000,
                memory_bytes: 8_000_000_000,
            },
            allocated: ResourceRequirements::default(),
            unschedulable: false,
            address: "127.0.0.1".to_string(),
            agent_api_port: 10250,
        }
    }

    fn make_pod(name: &str) -> Pod {
        Pod {
            id: format!("{}-id", name),
            name: name.to_string(),
            namespace: "default".to_string(),
            spec: PodSpec {
                containers: vec![ContainerSpec {
                    name: "app".to_string(),
                    image: "nginx:latest".to_string(),
                    command: vec![],
                    args: vec![],
                    env: HashMap::new(),
                    resources: ResourceRequirements {
                        cpu_millis: 100,
                        memory_bytes: 128_000_000,
                    },
                    volume_mounts: vec![],
                }],
                node_affinity: HashMap::new(),
                tolerations: vec![],
                volumes: vec![],
            },
            status: PodStatus::Pending,
            status_message: None,
            container_id: None,
            node_name: None,
            labels: HashMap::new(),
            owner_ref: None,
            restart_count: 0,
            runtime_info: None,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn test_schedule_round_robin() {
        let scheduler = Scheduler::new();
        let nodes = vec![
            make_node("node-1", NodeStatus::Ready),
            make_node("node-2", NodeStatus::Ready),
        ];
        let pod = make_pod("test-pod");

        let result1 = scheduler.schedule(&pod, &nodes);
        let result2 = scheduler.schedule(&pod, &nodes);

        assert!(result1.is_some());
        assert!(result2.is_some());
        assert_ne!(result1, result2); // Should alternate
    }

    #[test]
    fn test_skip_not_ready_nodes() {
        let scheduler = Scheduler::new();
        let nodes = vec![
            make_node("node-1", NodeStatus::NotReady),
            make_node("node-2", NodeStatus::Ready),
        ];
        let pod = make_pod("test-pod");

        let result = scheduler.schedule(&pod, &nodes);
        assert_eq!(result, Some("node-2-id".to_string()));
    }

    #[test]
    fn test_no_eligible_nodes() {
        let scheduler = Scheduler::new();
        let nodes = vec![
            make_node("node-1", NodeStatus::NotReady),
            make_node("node-2", NodeStatus::Unknown),
        ];
        let pod = make_pod("test-pod");

        let result = scheduler.schedule(&pod, &nodes);
        assert!(result.is_none());
    }
}
