//! VPC sync loop — periodically pulls VPC definitions and peerings from the server.

use std::sync::Arc;

use chrono::Utc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::allocator::GhostAllocator;
use crate::store::{VpcDaemonMeta, VpcStore};
use pkg_types::vpc::{Vpc, VpcPeering};
use pkg_vpc::constants::PLATFORM_PREFIX;

/// Start the VPC sync loop. Pulls from the server every `interval_secs` seconds.
pub fn start_sync_loop(
    server_url: String,
    token: String,
    store: Arc<VpcStore>,
    allocator: Arc<Mutex<GhostAllocator>>,
    interval_secs: u64,
) -> JoinHandle<()> {
    let client = reqwest::Client::new();

    tokio::spawn(async move {
        let mut interval =
            tokio::time::interval(tokio::time::Duration::from_secs(interval_secs));
        loop {
            interval.tick().await;

            let base = server_url.trim_end_matches('/');

            // Fetch VPCs
            let vpcs = match client
                .get(format!("{}/api/v1/vpcs", base))
                .header("Authorization", format!("Bearer {}", token))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => match resp.json::<Vec<Vpc>>().await {
                    Ok(v) => v,
                    Err(e) => {
                        warn!("VPC sync: failed to parse VPCs response: {}", e);
                        continue;
                    }
                },
                Ok(resp) => {
                    warn!("VPC sync: server returned {}", resp.status());
                    continue;
                }
                Err(e) => {
                    warn!("VPC sync: failed to reach server: {}", e);
                    continue;
                }
            };

            // Fetch peerings
            let peerings = match client
                .get(format!("{}/api/v1/vpc-peerings", base))
                .header("Authorization", format!("Bearer {}", token))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    match resp.json::<Vec<VpcPeering>>().await {
                        Ok(p) => p,
                        Err(e) => {
                            warn!("VPC sync: failed to parse peerings response: {}", e);
                            continue;
                        }
                    }
                }
                Ok(resp) => {
                    warn!("VPC sync: peerings endpoint returned {}", resp.status());
                    continue;
                }
                Err(e) => {
                    warn!("VPC sync: failed to fetch peerings: {}", e);
                    continue;
                }
            };

            // Save to store
            if let Err(e) = store.save_vpcs(&vpcs).await {
                warn!("VPC sync: failed to save VPCs: {}", e);
            }
            if let Err(e) = store.save_peerings(&peerings).await {
                warn!("VPC sync: failed to save peerings: {}", e);
            }

            // Sync allocator pools with latest VPC definitions
            {
                let mut alloc = allocator.lock().await;
                alloc.sync_vpcs(&vpcs);
            }

            // Update meta
            let existing_meta = store.load_meta().await.ok().flatten();
            let cluster_id = existing_meta.as_ref().and_then(|m| m.cluster_id);
            let meta = VpcDaemonMeta {
                cluster_id,
                platform_prefix: PLATFORM_PREFIX,
                last_synced_at: Utc::now(),
            };
            if let Err(e) = store.save_meta(&meta).await {
                warn!("VPC sync: failed to save meta: {}", e);
            }

            info!(
                "VPC sync: {} VPCs, {} peerings synced",
                vpcs.len(),
                peerings.len()
            );
        }
    })
}
