use crate::cache::AgentStateCache;
use crate::connectivity::ConnectivityManager;
use crate::registration;
use crate::store::AgentStore;
use pkg_types::node::NodeRegistrationRequest;
use std::sync::Arc;
use tracing::{info, warn};

/// Start the reconnect loop — probes and re-registers with the server when not connected.
#[allow(clippy::too_many_arguments)]
pub fn start(
    client: reqwest::Client,
    server: String,
    token: String,
    reg_req: NodeRegistrationRequest,
    node_name: String,
    cache: Arc<std::sync::RwLock<AgentStateCache>>,
    connectivity: Arc<ConnectivityManager>,
    store: AgentStore,
) {
    tokio::spawn(async move {
        let mut attempt = 0u32;
        loop {
            // When connected, idle and reset attempt counter
            if connectivity.is_connected() {
                attempt = 0;
                tokio::time::sleep(std::time::Duration::from_secs(pkg_constants::timings::RECONNECT_IDLE_SECS)).await;
                continue;
            }

            let delay = ConnectivityManager::backoff_duration(attempt);
            tokio::time::sleep(delay).await;

            let cached_node_id = cache.read().unwrap().node_id.clone();
            match registration::try_connect(
                &client,
                &server,
                &token,
                &reg_req,
                &node_name,
                cached_node_id.as_deref(),
            )
            .await
            {
                Ok(new_identity) => {
                    info!("Reconnected to server after {} attempts", attempt + 1);
                    if let Some((node_id, api_port)) = new_identity {
                        {
                            let mut c = cache.write().unwrap();
                            c.node_id = Some(node_id);
                            c.agent_api_port = Some(api_port);
                        }
                        let snapshot = cache.read().unwrap().clone();
                        if let Err(e) = store.save(&snapshot).await {
                            warn!("Failed to save to AgentStore after reconnect: {}", e);
                        }
                    }
                    connectivity.set_connected();
                    attempt = 0;
                }
                Err(e) => {
                    attempt += 1;
                    let age = cache.read().unwrap().age_secs();
                    warn!(
                        "Reconnect failed (attempt {}, cache age: {}s): {}",
                        attempt, age, e
                    );
                }
            }
        }
    });
}
