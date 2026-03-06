//! TAP device + iptables NAT setup for Firecracker VMs.
//!
//! Each VM gets a dedicated TAP device with a unique /30 subnet.
//! NAT masquerade is configured globally once.

use anyhow::Result;
use tracing::{info, warn};

/// Manages TAP devices and iptables NAT for Firecracker networking.
pub struct FcNetworkManager;

/// Well-known MAC address for the host TAP side.
/// Matches the static ARP entry configured by k3rs-init in the guest.
pub const TAP_WELL_KNOWN_MAC: &str = pkg_constants::network::GATEWAY_MAC_STR;

impl FcNetworkManager {
    /// Create and configure a TAP device for a VM.
    ///
    /// Each VM gets `tap-{short_id}` with a unique /30 subnet from 172.16.0.0/16.
    /// Returns the TAP device name.
    pub async fn setup_tap(id: &str, vm_index: u32) -> Result<String> {
        let short_id = &id[..8.min(id.len())];
        let tap_name = format!("tap-{}", short_id);

        // /30 subnets: 172.16.{vm_index}.0/30 gives .1 (host) and .2 (guest)
        let host_ip = format!("172.16.{}.1/30", vm_index);

        // Create TAP device
        let output = tokio::process::Command::new("ip")
            .args(["tuntap", "add", &tap_name, "mode", "tap"])
            .output()
            .await?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Ignore "already exists" errors
            if !stderr.contains("File exists") {
                anyhow::bail!(
                    "[fc-net] Failed to create TAP {}: {}",
                    tap_name,
                    stderr.trim()
                );
            }
        }

        // Assign IP
        let output = tokio::process::Command::new("ip")
            .args(["addr", "add", &host_ip, "dev", &tap_name])
            .output()
            .await?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.contains("File exists") {
                warn!("[fc-net] ip addr add warning: {}", stderr.trim());
            }
        }

        // Bring up
        let output = tokio::process::Command::new("ip")
            .args(["link", "set", &tap_name, "up"])
            .output()
            .await?;
        if !output.status.success() {
            warn!(
                "[fc-net] ip link set up warning: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }

        info!(
            "[fc-net] TAP {} created (host: {}, guest: 172.16.{}.2)",
            tap_name, host_ip, vm_index
        );
        Ok(tap_name)
    }

    /// Create and configure a TAP device for a VPC VM (pure IPv6 link).
    ///
    /// Unlike `setup_tap`, this does NOT assign an IPv4 address to the host side.
    /// The TAP carries only IPv6 — the VM does its own SIIT translation.
    /// Sets the well-known MAC so the guest's static ARP works.
    pub async fn setup_tap_vpc(id: &str) -> Result<String> {
        let short_id = &id[..8.min(id.len())];
        let tap_name = format!("tap-{}", short_id);

        // Create TAP device
        let output = tokio::process::Command::new("ip")
            .args(["tuntap", "add", &tap_name, "mode", "tap"])
            .output()
            .await?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.contains("File exists") {
                anyhow::bail!(
                    "[fc-net] Failed to create TAP {}: {}",
                    tap_name,
                    stderr.trim()
                );
            }
        }

        // Set well-known MAC address (matches guest static ARP)
        let output = tokio::process::Command::new("ip")
            .args(["link", "set", &tap_name, "address", TAP_WELL_KNOWN_MAC])
            .output()
            .await?;
        if !output.status.success() {
            warn!(
                "[fc-net] failed to set MAC on {}: {}",
                tap_name,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }

        // Bring up (no IPv4 address — pure IPv6 link)
        let output = tokio::process::Command::new("ip")
            .args(["link", "set", &tap_name, "up"])
            .output()
            .await?;
        if !output.status.success() {
            warn!(
                "[fc-net] ip link set up warning: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }

        info!(
            "[fc-net] TAP {} created (VPC mode, no host IPv4, MAC={})",
            tap_name, TAP_WELL_KNOWN_MAC
        );
        Ok(tap_name)
    }

    /// Setup iptables NAT masquerade for guest→internet connectivity.
    ///
    /// Called once globally, not per-VM. Idempotent.
    pub async fn setup_nat() -> Result<()> {
        // Enable IP forwarding
        if let Err(e) = tokio::fs::write("/proc/sys/net/ipv4/ip_forward", "1").await {
            warn!(
                "[fc-net] Failed to enable ip_forward: {} (may need root)",
                e
            );
        }

        // Detect default outbound interface
        let output = tokio::process::Command::new("ip")
            .args(["route", "show", "default"])
            .output()
            .await?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let default_iface = stdout
            .split_whitespace()
            .skip_while(|&w| w != "dev")
            .nth(1)
            .unwrap_or("eth0")
            .to_string();

        // Check if masquerade rule already exists
        let check = tokio::process::Command::new("iptables")
            .args([
                "-t",
                "nat",
                "-C",
                "POSTROUTING",
                "-o",
                &default_iface,
                "-j",
                "MASQUERADE",
            ])
            .output()
            .await;

        if let Ok(o) = check
            && o.status.success()
        {
            info!(
                "[fc-net] NAT masquerade already configured via {}",
                default_iface
            );
            return Ok(());
        }

        // Add masquerade rule
        let output = tokio::process::Command::new("iptables")
            .args([
                "-t",
                "nat",
                "-A",
                "POSTROUTING",
                "-o",
                &default_iface,
                "-j",
                "MASQUERADE",
            ])
            .output()
            .await?;

        if !output.status.success() {
            warn!(
                "[fc-net] iptables masquerade failed: {} (may need root)",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        } else {
            info!("[fc-net] NAT configured (masquerade via {})", default_iface);
        }

        Ok(())
    }

    /// Get the guest IP address for a given VM index.
    pub fn guest_ip(vm_index: u32) -> String {
        format!("172.16.{}.2", vm_index)
    }

    /// Get the host-side gateway IP for a given VM index.
    pub fn host_ip(vm_index: u32) -> String {
        format!("172.16.{}.1", vm_index)
    }

    /// Add a Ghost IPv6 address to an existing TAP device (VM dual-stack).
    ///
    /// Called from the agent after TAP setup to enable IPv6 connectivity
    /// for Firecracker VMs.
    pub async fn add_ipv6_to_tap(tap_name: &str, ghost_ipv6: &str) -> Result<()> {
        let addr_cidr = format!("{}/128", ghost_ipv6);
        let output = tokio::process::Command::new("ip")
            .args(["-6", "addr", "add", &addr_cidr, "dev", tap_name])
            .output()
            .await?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.contains("File exists") {
                anyhow::bail!(
                    "[fc-net] Failed to add IPv6 {} to {}: {}",
                    ghost_ipv6,
                    tap_name,
                    stderr.trim()
                );
            }
        }
        info!("[fc-net] IPv6 {} added to TAP {}", ghost_ipv6, tap_name);
        Ok(())
    }

    /// Cleanup TAP device when a VM is deleted.
    pub async fn cleanup_tap(id: &str) {
        let short_id = &id[..8.min(id.len())];
        let tap_name = format!("tap-{}", short_id);

        let _ = tokio::process::Command::new("ip")
            .args(["link", "delete", &tap_name])
            .output()
            .await;

        info!("[fc-net] TAP {} removed", tap_name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_guest_ip() {
        assert_eq!(FcNetworkManager::guest_ip(3), "172.16.3.2");
        assert_eq!(FcNetworkManager::guest_ip(42), "172.16.42.2");
    }

    #[test]
    fn test_host_ip() {
        assert_eq!(FcNetworkManager::host_ip(3), "172.16.3.1");
    }
}
