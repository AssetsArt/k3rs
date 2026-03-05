//! k3rs0 bridge manager — creates and manages the pod network bridge.
//!
//! Called once at agent startup (idempotent). All pods on the same node
//! attach their netkit host-side to this bridge for same-node connectivity.

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
            name: pkg_constants::network::BRIDGE_NAME.to_string(),
            gateway_ipv6: pkg_constants::network::BRIDGE_GATEWAY_IPV6.to_string(),
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
/// already exists. Always ensures the ghost route and DNS VIP are present.
pub async fn ensure_bridge(config: &BridgeConfig) -> Result<()> {
    let already_exists = bridge_exists(&config.name).await;

    if already_exists {
        info!("[bridge] {} already exists, skipping creation", config.name);
    }

    if !already_exists {
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
                warn!("[bridge] ip -6 addr add warning: {}", stderr.trim());
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

        // Disable multicast snooping so NDP solicitations (solicited-node multicast)
        // are flooded to all bridge ports. Without this, the bridge drops NDP for pods
        // that haven't sent MLD reports, breaking cross-pod IPv6 routing.
        let mcast_path = format!("/sys/class/net/{}/bridge/multicast_snooping", config.name);
        if let Err(e) = tokio::fs::write(&mcast_path, "0").await {
            warn!(
                "[bridge] Failed to disable multicast_snooping on {}: {}",
                config.name, e
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

    // Ensure DNS VIP on bridge so pods can reach the DNS server
    let dns_vip = pkg_constants::network::DNS_VIP;
    let dns_cidr = format!("{}/128", dns_vip);
    ensure_addr_or_route(&config.name, AddrOrRoute::Addr(&dns_cidr)).await;

    info!(
        "[bridge] {} ready (gateway: {}/64, route: {}, dns: {})",
        config.name, config.gateway_ipv6, ghost_prefix, dns_vip
    );
    Ok(())
}

enum AddrOrRoute<'a> {
    Addr(&'a str),
    Route(&'a str),
}

async fn ensure_addr_or_route(bridge: &str, kind: AddrOrRoute<'_>) {
    let (args, label): (Vec<&str>, &str) = match &kind {
        AddrOrRoute::Addr(cidr) => (
            vec!["-6", "addr", "add", cidr, "dev", bridge],
            cidr,
        ),
        AddrOrRoute::Route(prefix) => (
            vec!["-6", "route", "add", prefix, "dev", bridge],
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
