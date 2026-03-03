//! NoopEnforcer — log-only network enforcement backend.
//!
//! Used on platforms without nftables or eBPF (e.g. macOS development).
//! All operations succeed immediately with debug logging.

use anyhow::Result;
use async_trait::async_trait;
use tracing::debug;

use crate::enforcer::NetworkEnforcer;
use pkg_types::vpc::{Vpc, VpcPeering};

pub struct NoopEnforcer;

impl Default for NoopEnforcer {
    fn default() -> Self {
        Self::new()
    }
}

impl NoopEnforcer {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl NetworkEnforcer for NoopEnforcer {
    fn name(&self) -> &str {
        "noop"
    }

    async fn init(&mut self) -> Result<()> {
        debug!("noop: init (no-op)");
        Ok(())
    }

    async fn ensure_vpc(&mut self, vpc_id: u16, cidr: &str) -> Result<()> {
        debug!(vpc_id, cidr, "noop: ensure_vpc");
        Ok(())
    }

    async fn remove_vpc(&mut self, vpc_id: u16) -> Result<()> {
        debug!(vpc_id, "noop: remove_vpc");
        Ok(())
    }

    async fn install_pod_rules(
        &mut self,
        pod_id: &str,
        guest_ipv4: &str,
        vpc_id: u16,
    ) -> Result<()> {
        debug!(pod_id, guest_ipv4, vpc_id, "noop: install_pod_rules");
        Ok(())
    }

    async fn remove_pod_rules(&mut self, pod_id: &str) -> Result<()> {
        debug!(pod_id, "noop: remove_pod_rules");
        Ok(())
    }

    async fn install_tap_rules(
        &mut self,
        tap_name: &str,
        guest_ipv4: &str,
        vpc_id: u16,
    ) -> Result<()> {
        debug!(tap_name, guest_ipv4, vpc_id, "noop: install_tap_rules");
        Ok(())
    }

    async fn remove_tap_rules(&mut self, tap_name: &str) -> Result<()> {
        debug!(tap_name, "noop: remove_tap_rules");
        Ok(())
    }

    async fn install_veth_rules(
        &mut self,
        veth_name: &str,
        guest_ipv4: &str,
        vpc_id: u16,
    ) -> Result<()> {
        debug!(veth_name, guest_ipv4, vpc_id, "noop: install_veth_rules");
        Ok(())
    }

    async fn remove_veth_rules(&mut self, veth_name: &str) -> Result<()> {
        debug!(veth_name, "noop: remove_veth_rules");
        Ok(())
    }

    async fn install_peering_rules(&mut self, peering: &VpcPeering, _vpcs: &[Vpc]) -> Result<()> {
        debug!(peering.name, "noop: install_peering_rules");
        Ok(())
    }

    async fn remove_peering_rules(&mut self, peering_name: &str) -> Result<()> {
        debug!(peering_name, "noop: remove_peering_rules");
        Ok(())
    }

    async fn snapshot(&self) -> Result<String> {
        Ok("(noop)".to_string())
    }

    async fn cleanup(&mut self) -> Result<()> {
        debug!("noop: cleanup (no-op)");
        Ok(())
    }
}
