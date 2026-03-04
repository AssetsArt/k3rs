//! VPC sync loop — periodically pulls VPC definitions and peerings from the server.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::Utc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::allocator::GhostAllocator;
use crate::wireguard::WireGuardManager;
use k3rs_vpc::enforcer::NetworkEnforcer;
use k3rs_vpc::store::{VpcDaemonMeta, VpcStore};
use pkg_types::node::Node;
use pkg_types::vpc::{PeeringStatus, Vpc, VpcPeering};
use pkg_vpc::constants::PLATFORM_PREFIX;

/// Start the VPC sync loop. Pulls from the server every `interval_secs` seconds.
pub fn start_sync_loop(
    server_url: String,
    token: String,
    store: Arc<VpcStore>,
    allocator: Arc<Mutex<GhostAllocator>>,
    enforcer: Arc<Mutex<Box<dyn NetworkEnforcer>>>,
    wg_manager: Option<Arc<WireGuardManager>>,
    interval_secs: u64,
) -> JoinHandle<()> {
    let client = reqwest::Client::new();

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(interval_secs));

        // Track previous VPC IDs to detect removals
        let mut prev_vpc_ids: HashMap<u16, String> = HashMap::new();
        // Track previous peering names to detect removals
        let mut prev_peering_names: HashSet<String> = HashSet::new();

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

            // Sync enforcer VPC rules
            {
                let current_vpc_ids: HashMap<u16, String> = vpcs
                    .iter()
                    .map(|v| (v.vpc_id, v.ipv4_cidr.clone()))
                    .collect();

                let mut enf = enforcer.lock().await;

                // Ensure rules for new/existing VPCs
                for vpc in &vpcs {
                    if let Err(e) = enf.ensure_vpc(vpc.vpc_id, &vpc.ipv4_cidr).await {
                        warn!(
                            "VPC sync: failed to ensure VPC vpc_id={}: {}",
                            vpc.vpc_id, e
                        );
                    }
                }

                // Remove rules for VPCs that no longer exist
                for vpc_id in prev_vpc_ids.keys() {
                    if !current_vpc_ids.contains_key(vpc_id)
                        && let Err(e) = enf.remove_vpc(*vpc_id).await
                    {
                        warn!("VPC sync: failed to remove VPC vpc_id={}: {}", vpc_id, e);
                    }
                }

                prev_vpc_ids = current_vpc_ids;

                // Enforce peering rules
                let current_peering_names: HashSet<String> = peerings
                    .iter()
                    .filter(|p| p.status == PeeringStatus::Active)
                    .map(|p| p.name.clone())
                    .collect();

                // Remove rules for peerings that disappeared or became inactive
                for name in &prev_peering_names {
                    if !current_peering_names.contains(name)
                        && let Err(e) = enf.remove_peering_rules(name).await
                    {
                        warn!(
                            "VPC sync: failed to remove peering rules for '{}': {}",
                            name, e
                        );
                    }
                }

                // Install fresh rules for active peerings (remove old first for idempotency)
                for peering in peerings
                    .iter()
                    .filter(|p| p.status == PeeringStatus::Active)
                {
                    if let Err(e) = enf.remove_peering_rules(&peering.name).await {
                        warn!(
                            "VPC sync: failed to remove old peering rules for '{}': {}",
                            peering.name, e
                        );
                    }
                    if let Err(e) = enf.install_peering_rules(peering, &vpcs).await {
                        warn!(
                            "VPC sync: failed to install peering rules for '{}': {}",
                            peering.name, e
                        );
                    }
                }

                prev_peering_names = current_peering_names;
            }

            // Sync WireGuard mesh peers from node list
            if let Some(ref wg_mgr) = wg_manager {
                match client
                    .get(format!("{}/api/v1/nodes", base))
                    .header("Authorization", format!("Bearer {}", token))
                    .send()
                    .await
                {
                    Ok(resp) if resp.status().is_success() => {
                        match resp.json::<Vec<Node>>().await {
                            Ok(nodes) => {
                                if let Err(e) = wg_mgr.sync_peers(&nodes).await {
                                    warn!("VPC sync: WireGuard peer sync failed: {}", e);
                                }
                            }
                            Err(e) => {
                                warn!("VPC sync: failed to parse nodes response: {}", e);
                            }
                        }
                    }
                    Ok(resp) => {
                        warn!("VPC sync: nodes endpoint returned {}", resp.status());
                    }
                    Err(e) => {
                        warn!("VPC sync: failed to fetch nodes: {}", e);
                    }
                }
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
