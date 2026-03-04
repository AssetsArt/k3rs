//! NetworkEnforcer trait — abstract interface for VPC network isolation backends.
//!
//! Implementations: EbpfEnforcer (TC classifier via Aya), NoopEnforcer (log-only).

use anyhow::Result;
use async_trait::async_trait;
use tracing::warn;

use crate::store::StoredAllocation;
use pkg_types::vpc::{Vpc, VpcPeering};

/// Pluggable network enforcement backend.
/// All methods take `&mut self` — objects are held behind `Arc<Mutex<>>`.
#[async_trait]
pub trait NetworkEnforcer: Send {
    /// Human-readable backend name (e.g. "ebpf", "noop").
    fn name(&self) -> &str;

    /// One-time initialization (create tables, load BPF programs, etc).
    async fn init(&mut self) -> Result<()>;

    /// Ensure VPC-level isolation structures exist for this VPC.
    async fn ensure_vpc(&mut self, vpc_id: u16, cidr: &str) -> Result<()>;

    /// Remove VPC-level isolation structures.
    async fn remove_vpc(&mut self, vpc_id: u16) -> Result<()>;

    /// Install TAP-interface-specific rules (VM traffic).
    /// Attaches `tap_guard` (anti-spoofing) on TAP ingress and IPv6 isolation classifiers
    /// on TAP egress+ingress. No IPv4 classifiers — the VM does its own SIIT.
    async fn install_tap_rules(
        &mut self,
        tap_name: &str,
        guest_ipv4: &str,
        ghost_ipv6: &str,
        vpc_id: u16,
        vpc_cidr: &str,
    ) -> Result<()>;

    /// Remove all rules associated with a TAP interface.
    async fn remove_tap_rules(&mut self, tap_name: &str) -> Result<()>;

    /// Install netkit-interface-specific rules (OCI container traffic).
    /// Attaches SIIT translators (IPv4↔IPv6) inside the pod's netns on eth0,
    /// and IPv6 VPC isolation classifiers on the host-side netkit.
    async fn install_netkit_rules(
        &mut self,
        nk_name: &str,
        guest_ipv4: &str,
        ghost_ipv6: &str,
        vpc_id: u16,
        vpc_cidr: &str,
        container_pid: u32,
    ) -> Result<()>;

    /// Remove all rules associated with a netkit interface.
    async fn remove_netkit_rules(&mut self, nk_name: &str) -> Result<()>;

    /// Install cross-VPC peering accept rules.
    async fn install_peering_rules(&mut self, peering: &VpcPeering, vpcs: &[Vpc]) -> Result<()>;

    /// Remove all rules associated with a peering by name.
    async fn remove_peering_rules(&mut self, peering_name: &str) -> Result<()>;

    /// Install NAT64 translation programs on the bridge and physical interfaces.
    async fn install_nat64(
        &mut self,
        _node_ipv4: &str,
        _bridge_name: &str,
        _phys_name: &str,
    ) -> Result<()> {
        Ok(()) // default no-op
    }

    /// Remove NAT64 translation state.
    async fn remove_nat64(&mut self) -> Result<()> {
        Ok(()) // default no-op
    }

    /// Return a human-readable snapshot of current enforcement state.
    async fn snapshot(&self) -> Result<String>;

    /// Tear down all enforcement state (tables, programs, maps).
    async fn cleanup(&mut self) -> Result<()>;

    /// Rebuild all enforcement state from stored VPCs, allocations, and peerings.
    /// Default implementation composes ensure_vpc + install_tap_rules + install_peering_rules.
    async fn rebuild(
        &mut self,
        vpcs: &[Vpc],
        allocations: &[StoredAllocation],
        peerings: &[VpcPeering],
    ) -> Result<()> {
        for vpc in vpcs {
            if let Err(e) = self.ensure_vpc(vpc.vpc_id, &vpc.ipv4_cidr).await {
                warn!(
                    "{}: failed to ensure VPC {} (id={}): {}",
                    self.name(),
                    vpc.name,
                    vpc.vpc_id,
                    e
                );
            }
        }

        for alloc in allocations {
            // TAP allocations need install_tap_rules for guard + IPv6 isolation classifiers.
            // Note: netkit rules are not rebuilt here because they require container_pid
            // (the pod must be running). The agent re-attaches on pod start.
            if alloc.interface_type == "tap" {
                let vpc_cidr = vpcs
                    .iter()
                    .find(|v| v.vpc_id == alloc.vpc_id)
                    .map(|v| v.ipv4_cidr.as_str())
                    .unwrap_or("0.0.0.0/0");
                let short_id = &alloc.pod_id[..8.min(alloc.pod_id.len())];
                let tap_name = format!("tap-{}", short_id);
                if let Err(e) = self
                    .install_tap_rules(
                        &tap_name,
                        &alloc.guest_ipv4,
                        &alloc.ghost_ipv6,
                        alloc.vpc_id,
                        vpc_cidr,
                    )
                    .await
                {
                    warn!(
                        "{}: failed to install TAP rules for {} (tap={}): {}",
                        self.name(),
                        alloc.pod_id,
                        tap_name,
                        e
                    );
                }
            }
        }

        for peering in peerings {
            if let Err(e) = self.install_peering_rules(peering, vpcs).await {
                warn!(
                    "{}: failed to install peering rules for '{}': {}",
                    self.name(),
                    peering.name,
                    e
                );
            }
        }

        Ok(())
    }
}
