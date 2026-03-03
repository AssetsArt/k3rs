//! NetworkEnforcer trait — abstract interface for VPC network isolation backends.
//!
//! Implementations: NftManager (nftables CLI), NoopEnforcer (log-only),
//! EbpfEnforcer (TC classifier via Aya).

use anyhow::Result;
use async_trait::async_trait;
use tracing::warn;

use crate::store::StoredAllocation;
use pkg_types::vpc::{Vpc, VpcPeering};

/// Pluggable network enforcement backend.
/// All methods take `&mut self` — objects are held behind `Arc<Mutex<>>`.
#[async_trait]
pub trait NetworkEnforcer: Send {
    /// Human-readable backend name (e.g. "nftables", "ebpf", "noop").
    fn name(&self) -> &str;

    /// One-time initialization (create tables, load BPF programs, etc).
    async fn init(&mut self) -> Result<()>;

    /// Ensure VPC-level isolation structures exist for this VPC.
    async fn ensure_vpc(&mut self, vpc_id: u16, cidr: &str) -> Result<()>;

    /// Remove VPC-level isolation structures.
    async fn remove_vpc(&mut self, vpc_id: u16) -> Result<()>;

    /// Install per-pod forwarding and anti-spoofing rules.
    async fn install_pod_rules(
        &mut self,
        pod_id: &str,
        guest_ipv4: &str,
        vpc_id: u16,
    ) -> Result<()>;

    /// Remove all rules associated with a pod.
    async fn remove_pod_rules(&mut self, pod_id: &str) -> Result<()>;

    /// Install TAP-interface-specific rules (VM traffic).
    async fn install_tap_rules(
        &mut self,
        tap_name: &str,
        guest_ipv4: &str,
        vpc_id: u16,
    ) -> Result<()>;

    /// Remove all rules associated with a TAP interface.
    async fn remove_tap_rules(&mut self, tap_name: &str) -> Result<()>;

    /// Install veth-interface-specific rules (OCI container traffic).
    /// Attaches TC classifiers (IPv4 + IPv6) to the host-side veth.
    async fn install_veth_rules(
        &mut self,
        veth_name: &str,
        guest_ipv4: &str,
        vpc_id: u16,
    ) -> Result<()>;

    /// Remove all rules associated with a veth interface.
    async fn remove_veth_rules(&mut self, veth_name: &str) -> Result<()>;

    /// Install cross-VPC peering accept rules.
    async fn install_peering_rules(&mut self, peering: &VpcPeering, vpcs: &[Vpc]) -> Result<()>;

    /// Remove all rules associated with a peering by name.
    async fn remove_peering_rules(&mut self, peering_name: &str) -> Result<()>;

    /// Install NAT64 translation programs on the bridge and physical interfaces.
    /// Populates the NAT64_CONFIG BPF map and attaches nat64_egress (bridge egress)
    /// and nat64_ingress (physical ingress) TC classifiers.
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
    /// Default implementation composes ensure_vpc + install_pod_rules + install_peering_rules.
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
            if let Err(e) = self
                .install_pod_rules(&alloc.pod_id, &alloc.guest_ipv4, alloc.vpc_id)
                .await
            {
                warn!(
                    "{}: failed to install pod rules for {} in VPC {}: {}",
                    self.name(),
                    alloc.pod_id,
                    alloc.vpc_name,
                    e
                );
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
