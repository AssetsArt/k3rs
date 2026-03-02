use pkg_container::ContainerRuntime;
use std::sync::Arc;
use tracing::{info, warn};

/// Run agent recovery: adopt desired containers, stop orphaned ones.
pub async fn run_recovery(
    runtime: &Arc<ContainerRuntime>,
    node_id: Option<&str>,
    server: &str,
    node_name: &str,
    token: &str,
    client: &reqwest::Client,
    cached_pods: Vec<pkg_types::pod::Pod>,
) {
    info!("Starting agent recovery procedure...");
    let discovered = runtime
        .discover_running_containers()
        .await
        .unwrap_or_default();

    if node_id.is_some() {
        let url = format!(
            "{}/api/v1/pods?fieldSelector=spec.nodeName={}",
            server.trim_end_matches('/'),
            node_name
        );
        let desired_pods: Vec<pkg_types::pod::Pod> = match client
            .get(&url)
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await
        {
            Ok(resp) => resp.json().await.unwrap_or_default(),
            Err(e) => {
                warn!("Agent recovery: failed to fetch desired pods: {}", e);
                // Use cached pods if server unreachable
                cached_pods
            }
        };

        let mut desired_running_ids = std::collections::HashMap::new();
        for pod in &desired_pods {
            desired_running_ids.insert(pod.id.clone(), (pod.name.clone(), pod.namespace.clone()));
        }

        for cid in discovered {
            if let Some((pod_name, pod_ns)) = desired_running_ids.get(&cid) {
                info!("Agent recovery: adopting desired container {}", cid);
                let status_url = format!(
                    "{}/api/v1/namespaces/{}/pods/{}/status",
                    server.trim_end_matches('/'),
                    pod_ns,
                    pod_name
                );
                let _ = client
                    .put(&status_url)
                    .header("Authorization", format!("Bearer {}", token))
                    .json(&pkg_types::pod::PodStatus::Running)
                    .send()
                    .await;
            } else {
                info!("Agent recovery: stopping orphaned container {}", cid);
                let _ = runtime.cleanup_container(&cid).await;
            }
        }
    } else {
        info!("Agent recovery: no node_id, skipping pod reconciliation");
        // Still cleanup any orphaned containers
        for cid in discovered {
            info!("Agent recovery: stopping orphaned container {}", cid);
            let _ = runtime.cleanup_container(&cid).await;
        }
    }
    info!("Agent recovery complete.");
}
