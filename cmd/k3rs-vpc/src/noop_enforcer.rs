//! NoopEnforcer — log-only network enforcement backend.
//!
//! Used on platforms without nftables or eBPF (e.g. macOS development).
//! All operations succeed immediately with debug logging.

use anyhow::Result;
use async_trait::async_trait;
use tracing::debug;

use crate::enforcer::NetworkEnforcer;
use pkg_types::vpc::{Vpc, VpcPeering};

#[allow(dead_code)]
pub struct NoopEnforcer;

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
        debug!("noop: ensure_vpc vpc_id={} cidr={}", vpc_id, cidr);
        Ok(())
    }

    async fn remove_vpc(&mut self, vpc_id: u16) -> Result<()> {
        debug!("noop: remove_vpc vpc_id={}", vpc_id);
        Ok(())
    }

    async fn install_pod_rules(
        &mut self,
        pod_id: &str,
        guest_ipv4: &str,
        vpc_id: u16,
    ) -> Result<()> {
        debug!(
            "noop: install_pod_rules pod={} ipv4={} vpc_id={}",
            pod_id, guest_ipv4, vpc_id
        );
        Ok(())
    }

    async fn remove_pod_rules(&mut self, pod_id: &str) -> Result<()> {
        debug!("noop: remove_pod_rules pod={}", pod_id);
        Ok(())
    }

    async fn install_tap_rules(
        &mut self,
        tap_name: &str,
        guest_ipv4: &str,
        vpc_id: u16,
    ) -> Result<()> {
        debug!(
            "noop: install_tap_rules tap={} ipv4={} vpc_id={}",
            tap_name, guest_ipv4, vpc_id
        );
        Ok(())
    }

    async fn remove_tap_rules(&mut self, tap_name: &str) -> Result<()> {
        debug!("noop: remove_tap_rules tap={}", tap_name);
        Ok(())
    }

    async fn install_peering_rules(&mut self, peering: &VpcPeering, _vpcs: &[Vpc]) -> Result<()> {
        debug!("noop: install_peering_rules peering={}", peering.name);
        Ok(())
    }

    async fn remove_peering_rules(&mut self, peering_name: &str) -> Result<()> {
        debug!("noop: remove_peering_rules peering={}", peering_name);
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
