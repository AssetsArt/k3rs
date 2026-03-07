use crate::cache::AgentStateCache;
use crate::connectivity::ConnectivityManager;
use pkg_container::ContainerRuntime;
use std::sync::Arc;
use tracing::warn;

/// Start the image reporting loop (every 30s).
pub fn start(
    runtime: Option<Arc<ContainerRuntime>>,
    client: reqwest::Client,
    server: String,
    token: String,
    cache: Arc<std::sync::RwLock<AgentStateCache>>,
    connectivity: Arc<ConnectivityManager>,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(
            pkg_constants::timings::IMAGE_REPORT_INTERVAL_SECS,
        ));
        loop {
            interval.tick().await;

            // Skip when not connected
            if !connectivity.is_connected() {
                continue;
            }

            let node_id = cache.read().unwrap().node_id.clone();
            let Some(ref node_id) = node_id else {
                continue;
            };

            let Some(ref rt) = runtime else {
                continue;
            };
            match rt.list_images().await {
                Ok(images) => {
                    let url = format!(
                        "{}/api/v1/nodes/{}/images",
                        server.trim_end_matches('/'),
                        node_id
                    );
                    let _ = client
                        .put(&url)
                        .header("Authorization", format!("Bearer {}", token))
                        .json(&images)
                        .send()
                        .await;
                }
                Err(e) => warn!("Failed to list images: {}", e),
            }
        }
    });
}
