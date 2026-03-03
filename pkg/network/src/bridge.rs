//! k3rs0 bridge manager — creates and manages the pod network bridge.
//!
//! Called once at agent startup (idempotent). All pods on the same node
//! attach their veth host-side to this bridge for same-node connectivity.

use anyhow::Result;
use tracing::{info, warn};

/// Configuration for the k3rs0 bridge.
pub struct BridgeConfig {
    /// Bridge interface name (default: `k3rs0`).
    pub name: String,
    /// Link-local IPv6 gateway assigned to the bridge.
    pub gateway_ipv6: String,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            name: "k3rs0".to_string(),
            gateway_ipv6: "fe80::1".to_string(),
        }
    }
}

/// Check whether a bridge interface exists.
pub async fn bridge_exists(name: &str) -> bool {
    tokio::process::Command::new("ip")
        .args(["link", "show", name])
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Create the k3rs0 bridge, assign the link-local gateway, bring it up,
/// and enable IPv6 forwarding. Idempotent — skips creation if the bridge
/// already exists.
pub async fn ensure_bridge(config: &BridgeConfig) -> Result<()> {
    // Skip if bridge already exists
    if bridge_exists(&config.name).await {
        info!("[bridge] {} already exists, skipping creation", config.name);
        return Ok(());
    }

    // Create bridge
    let output = tokio::process::Command::new("ip")
        .args(["link", "add", &config.name, "type", "bridge"])
        .output()
        .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.contains("File exists") {
            anyhow::bail!(
                "[bridge] Failed to create {}: {}",
                config.name,
                stderr.trim()
            );
        }
    }

    // Assign link-local IPv6 gateway
    let gw_cidr = format!("{}/64", config.gateway_ipv6);
    let output = tokio::process::Command::new("ip")
        .args(["-6", "addr", "add", &gw_cidr, "dev", &config.name])
        .output()
        .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.contains("File exists") {
            warn!(
                "[bridge] ip -6 addr add warning: {}",
                stderr.trim()
            );
        }
    }

    // Bring bridge up
    let output = tokio::process::Command::new("ip")
        .args(["link", "set", &config.name, "up"])
        .output()
        .await?;
    if !output.status.success() {
        warn!(
            "[bridge] ip link set up warning: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    // Enable IPv6 forwarding
    let sysctl_path = format!("/proc/sys/net/ipv6/conf/{}/forwarding", config.name);
    if let Err(e) = tokio::fs::write(&sysctl_path, "1").await {
        warn!(
            "[bridge] Failed to enable IPv6 forwarding on {}: {} (may need root)",
            config.name, e
        );
    }
    // Also enable globally
    if let Err(e) = tokio::fs::write("/proc/sys/net/ipv6/conf/all/forwarding", "1").await {
        warn!(
            "[bridge] Failed to enable global IPv6 forwarding: {} (may need root)",
            e
        );
    }

    info!(
        "[bridge] {} created (gateway: {}/64, IPv6 forwarding enabled)",
        config.name, config.gateway_ipv6
    );
    Ok(())
}
