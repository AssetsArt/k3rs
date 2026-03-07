use crate::cache::AgentStateCache;
use crate::connectivity::ConnectivityManager;
use std::sync::Arc;
use tracing::{info, warn};

/// Start the heartbeat loop on a dedicated OS thread with its own tokio runtime.
pub fn start_heartbeat_loop(
    server_base: String,
    node_name: String,
    token: String,
    connectivity: Arc<ConnectivityManager>,
    cache: Arc<std::sync::RwLock<AgentStateCache>>,
) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("heartbeat runtime");
        rt.block_on(async move {
            let client = reqwest::Client::new();
            let mut fail_count = 0u32;
            loop {
                // Connected: poll every 10s. Failing: exponential backoff 1s→2s→4s→30s.
                //
                // `fail_count` is incremented *after* each failure, so it is
                // 1-based at the top of the loop. We subtract 1 to convert to
                // the 0-based index that `backoff_duration` expects, ensuring
                // the first retry fires after 1s (not 2s).
                let delay = if fail_count == 0 {
                    std::time::Duration::from_secs(pkg_constants::timings::HEARTBEAT_INTERVAL_SECS)
                } else {
                    ConnectivityManager::backoff_duration(fail_count.saturating_sub(1))
                };
                tokio::time::sleep(delay).await;

                // Skip heartbeat if we have no node_id yet (never registered)
                let has_node_id = cache.read().unwrap().node_id.is_some();
                if !has_node_id {
                    continue;
                }

                let url = format!(
                    "{}/api/v1/nodes/{}/heartbeat",
                    server_base.trim_end_matches('/'),
                    node_name
                );
                match client
                    .put(&url)
                    .header("Authorization", format!("Bearer {}", token))
                    .timeout(std::time::Duration::from_secs(
                        pkg_constants::timings::HEARTBEAT_TIMEOUT_SECS,
                    ))
                    .send()
                    .await
                {
                    Ok(resp) if resp.status().is_success() => {
                        if fail_count > 0 {
                            info!("Heartbeat recovered after {} failures", fail_count);
                        }
                        fail_count = 0;
                        connectivity.set_connected();
                        info!("Heartbeat OK for {} (status=200)", node_name,);
                    }
                    Ok(resp) => {
                        fail_count += 1;
                        warn!(
                            "Heartbeat failed for {} (status={})",
                            node_name,
                            resp.status()
                        );
                        let age = cache.read().unwrap().age_secs();
                        connectivity.set_reconnecting(fail_count);
                        warn!(
                            "Server unreachable, retrying (attempt {}, cache age: {}s)",
                            fail_count, age
                        );
                    }
                    Err(e) => {
                        fail_count += 1;
                        warn!("Heartbeat failed for {}: {}", node_name, e);
                        let age = cache.read().unwrap().age_secs();
                        connectivity.set_reconnecting(fail_count);
                        warn!(
                            "Server unreachable, retrying (attempt {}, cache age: {}s)",
                            fail_count, age
                        );
                    }
                }
            }
        });
    });
    info!("Heartbeat loop started");
}
