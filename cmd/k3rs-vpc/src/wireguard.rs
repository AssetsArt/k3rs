//! WireGuard mesh manager — maintains the `wg-k3rs` interface and peers
//! for cross-node pod-to-pod traffic.

use std::collections::HashSet;

use anyhow::Result;
use pkg_network::wireguard;
use pkg_types::node::Node;
use tracing::{info, warn};

/// Manages the WireGuard mesh interface and peer synchronization.
pub struct WireGuardManager {
    #[allow(dead_code)]
    private_key: String,
    public_key: String,
    listen_port: u16,
    #[allow(dead_code)]
    key_path: String,
}

impl WireGuardManager {
    /// Load or generate a WireGuard keypair, persist to disk, and create the interface.
    pub async fn init(listen_port: u16, key_path: &str) -> Result<Self> {
        tokio::fs::create_dir_all(key_path).await?;

        let priv_path = format!("{}/private.key", key_path);
        let pub_path = format!("{}/public.key", key_path);

        let (private_key, public_key) = if tokio::fs::try_exists(&priv_path).await.unwrap_or(false)
        {
            let private_key = tokio::fs::read_to_string(&priv_path)
                .await?
                .trim()
                .to_string();
            let public_key = tokio::fs::read_to_string(&pub_path)
                .await?
                .trim()
                .to_string();
            info!(
                "[wg-manager] Loaded existing keypair (pubkey: {}...)",
                &public_key[..8.min(public_key.len())]
            );
            (private_key, public_key)
        } else {
            let (priv_key, pub_key) = wireguard::generate_keypair().await?;
            tokio::fs::write(&priv_path, &priv_key).await?;
            tokio::fs::write(&pub_path, &pub_key).await?;
            info!(
                "[wg-manager] Generated and persisted new keypair to {}",
                key_path
            );
            (priv_key, pub_key)
        };

        // Create and configure the WireGuard interface
        wireguard::ensure_wireguard(listen_port, &private_key).await?;

        // Add the Ghost IPv6 route
        wireguard::ensure_ghost_route().await?;

        Ok(Self {
            private_key,
            public_key,
            listen_port,
            key_path: key_path.to_string(),
        })
    }

    /// Get this node's WireGuard public key.
    pub fn public_key(&self) -> &str {
        &self.public_key
    }

    /// Get the configured listen port.
    pub fn listen_port(&self) -> u16 {
        self.listen_port
    }

    /// Synchronize WireGuard peers with the current node list.
    /// Adds peers for new nodes, removes peers for departed nodes.
    /// Skips self (by matching public key).
    pub async fn sync_peers(&self, nodes: &[Node]) -> Result<()> {
        let current_peers = wireguard::list_peers().await?;
        let current_keys: HashSet<String> =
            current_peers.iter().map(|p| p.public_key.clone()).collect();

        // Build desired peer set from nodes that have WG info and aren't us
        let mut desired_keys: HashSet<String> = HashSet::new();
        for node in nodes {
            let Some(ref pub_key) = node.wg_public_key else {
                continue;
            };
            if pub_key == &self.public_key {
                continue; // Skip self
            }
            let Some(ref endpoint) = node.wg_endpoint else {
                continue;
            };

            desired_keys.insert(pub_key.clone());

            // Add peer if not already present
            if !current_keys.contains(pub_key) {
                // AllowedIPs = ::/0 — VPC isolation is handled by eBPF, not WireGuard
                if let Err(e) = wireguard::add_peer(pub_key, endpoint, "::/0").await {
                    warn!(
                        "[wg-manager] Failed to add peer {} (node {}): {}",
                        &pub_key[..8.min(pub_key.len())],
                        node.name,
                        e
                    );
                }
            }
        }

        // Remove peers that are no longer in the node list
        for peer in &current_peers {
            if !desired_keys.contains(&peer.public_key) {
                if let Err(e) = wireguard::remove_peer(&peer.public_key).await {
                    warn!(
                        "[wg-manager] Failed to remove peer {}: {}",
                        &peer.public_key[..8.min(peer.public_key.len())],
                        e
                    );
                }
            }
        }

        Ok(())
    }
}
