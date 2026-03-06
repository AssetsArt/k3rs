//! k3rs0 dummy device manager — creates and manages the pod network anchor device.
//!
//! Called once at agent startup (idempotent). The dummy device hosts the DNS VIP
//! (`fd6b:3372::53`) and serves as the routing anchor for the ghost prefix
//! (`fd6b:3372::/32`) and NAT64 egress (`64:ff9b::/96`).
//!
//! Pod-to-pod traffic does NOT flow through this device — it uses BPF
//! `redirect_peer` for same-node delivery or WireGuard for cross-node.

use anyhow::Result;
use tracing::{info, warn};

/// Configuration for the k3rs0 dummy device.
pub struct BridgeConfig {
    /// Device name (default: `k3rs0`).
    pub name: String,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            name: pkg_constants::network::BRIDGE_NAME.to_string(),
        }
    }
}

/// Check whether the device exists.
pub async fn bridge_exists(name: &str) -> bool {
    tokio::process::Command::new("ip")
        .args(["link", "show", name])
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Create the k3rs0 dummy device, bring it up, and enable IPv6 forwarding.
/// Idempotent — skips creation if the device already exists.
/// Always ensures the ghost route and DNS VIP are present.
pub async fn ensure_bridge(config: &BridgeConfig) -> Result<()> {
    let already_exists = bridge_exists(&config.name).await;

    if already_exists {
        info!("[bridge] {} already exists, skipping creation", config.name);
    }

    if !already_exists {
        // Create dummy device (replaces bridge — no L2 switching needed)
        let output = tokio::process::Command::new("ip")
            .args(["link", "add", &config.name, "type", "dummy"])
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

        // Bring device up
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
        if let Err(e) = tokio::fs::write("/proc/sys/net/ipv6/conf/all/forwarding", "1").await {
            warn!(
                "[bridge] Failed to enable global IPv6 forwarding: {} (may need root)",
                e
            );
        }
    }

    // Ensure ghost route (idempotent — always run in case route was lost)
    let ghost_prefix = crate::wireguard::GHOST_ROUTE_PREFIX;
    ensure_addr_or_route(&config.name, AddrOrRoute::Route(ghost_prefix)).await;

    // Ensure DNS VIP on device so pods can reach the DNS server
    let dns_vip = pkg_constants::network::DNS_VIP;
    let dns_cidr = format!("{}/128", dns_vip);
    ensure_addr_or_route(&config.name, AddrOrRoute::Addr(&dns_cidr)).await;

    info!(
        "[bridge] {} ready (route: {}, dns: {})",
        config.name, ghost_prefix, dns_vip
    );
    Ok(())
}

enum AddrOrRoute<'a> {
    Addr(&'a str),
    Route(&'a str),
}

async fn ensure_addr_or_route(dev: &str, kind: AddrOrRoute<'_>) {
    let (args, label): (Vec<&str>, &str) = match &kind {
        AddrOrRoute::Addr(cidr) => (
            vec!["-6", "addr", "add", cidr, "dev", dev],
            cidr,
        ),
        AddrOrRoute::Route(prefix) => (
            vec!["-6", "route", "add", prefix, "dev", dev],
            prefix,
        ),
    };
    let output = tokio::process::Command::new("ip")
        .args(&args)
        .output()
        .await;
    match output {
        Ok(o) if !o.status.success() => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            if !stderr.contains("File exists") {
                warn!("[bridge] ip {} failed: {}", label, stderr.trim());
            }
        }
        Err(e) => warn!("[bridge] ip {} failed: {}", label, e),
        _ => {}
    }
}
