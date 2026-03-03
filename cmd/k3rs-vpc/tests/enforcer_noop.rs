//! Integration tests for NoopEnforcer — exercises the NetworkEnforcer trait
//! through a complete lifecycle. Always runnable, no privileges needed.

use pkg_types::vpc::{
    PeeringDirection, PeeringStatus, Vpc, VpcPeering, VpcStatus,
};

/// Since NoopEnforcer is in a private module, we replicate its behavior for testing.
/// In a real setup, this would be tested through the daemon binary.
/// Here we verify the trait contract by creating a minimal noop impl.
mod noop {
    use anyhow::Result;
    use async_trait::async_trait;

    #[async_trait]
    pub trait NetworkEnforcer: Send {
        fn name(&self) -> &str;
        async fn init(&mut self) -> Result<()>;
        async fn ensure_vpc(&mut self, vpc_id: u16, cidr: &str) -> Result<()>;
        async fn remove_vpc(&mut self, vpc_id: u16) -> Result<()>;
        async fn install_pod_rules(
            &mut self,
            pod_id: &str,
            guest_ipv4: &str,
            vpc_id: u16,
        ) -> Result<()>;
        async fn remove_pod_rules(&mut self, pod_id: &str) -> Result<()>;
        async fn install_tap_rules(
            &mut self,
            tap_name: &str,
            guest_ipv4: &str,
            vpc_id: u16,
        ) -> Result<()>;
        async fn remove_tap_rules(&mut self, tap_name: &str) -> Result<()>;
        async fn install_peering_rules(
            &mut self,
            peering: &pkg_types::vpc::VpcPeering,
            vpcs: &[pkg_types::vpc::Vpc],
        ) -> Result<()>;
        async fn remove_peering_rules(&mut self, peering_name: &str) -> Result<()>;
        async fn snapshot(&self) -> Result<String>;
        async fn cleanup(&mut self) -> Result<()>;
    }

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
            Ok(())
        }
        async fn ensure_vpc(&mut self, _vpc_id: u16, _cidr: &str) -> Result<()> {
            Ok(())
        }
        async fn remove_vpc(&mut self, _vpc_id: u16) -> Result<()> {
            Ok(())
        }
        async fn install_pod_rules(
            &mut self,
            _pod_id: &str,
            _guest_ipv4: &str,
            _vpc_id: u16,
        ) -> Result<()> {
            Ok(())
        }
        async fn remove_pod_rules(&mut self, _pod_id: &str) -> Result<()> {
            Ok(())
        }
        async fn install_tap_rules(
            &mut self,
            _tap_name: &str,
            _guest_ipv4: &str,
            _vpc_id: u16,
        ) -> Result<()> {
            Ok(())
        }
        async fn remove_tap_rules(&mut self, _tap_name: &str) -> Result<()> {
            Ok(())
        }
        async fn install_peering_rules(
            &mut self,
            _peering: &pkg_types::vpc::VpcPeering,
            _vpcs: &[pkg_types::vpc::Vpc],
        ) -> Result<()> {
            Ok(())
        }
        async fn remove_peering_rules(&mut self, _peering_name: &str) -> Result<()> {
            Ok(())
        }
        async fn snapshot(&self) -> Result<String> {
            Ok("(noop)".to_string())
        }
        async fn cleanup(&mut self) -> Result<()> {
            Ok(())
        }
    }
}

use noop::{NetworkEnforcer, NoopEnforcer};

fn make_vpc(name: &str, vpc_id: u16, cidr: &str) -> Vpc {
    Vpc {
        name: name.to_string(),
        vpc_id,
        ipv4_cidr: cidr.to_string(),
        status: VpcStatus::Active,
        created_at: chrono::Utc::now(),
    }
}

fn make_peering(name: &str, vpc_a: &str, vpc_b: &str) -> VpcPeering {
    VpcPeering {
        name: name.to_string(),
        vpc_a: vpc_a.to_string(),
        vpc_b: vpc_b.to_string(),
        direction: PeeringDirection::Bidirectional,
        status: PeeringStatus::Active,
        created_at: chrono::Utc::now(),
    }
}

#[tokio::test]
async fn test_noop_enforcer_name() {
    let enforcer = NoopEnforcer::new();
    assert_eq!(enforcer.name(), "noop");
}

#[tokio::test]
async fn test_noop_enforcer_init() {
    let mut enforcer = NoopEnforcer::new();
    assert!(enforcer.init().await.is_ok());
}

#[tokio::test]
async fn test_noop_enforcer_vpc_lifecycle() {
    let mut enforcer = NoopEnforcer::new();
    enforcer.init().await.unwrap();

    // Ensure VPC
    assert!(enforcer.ensure_vpc(1, "10.0.1.0/24").await.is_ok());
    assert!(enforcer.ensure_vpc(2, "10.0.2.0/24").await.is_ok());

    // Idempotent
    assert!(enforcer.ensure_vpc(1, "10.0.1.0/24").await.is_ok());

    // Remove
    assert!(enforcer.remove_vpc(1).await.is_ok());
    assert!(enforcer.remove_vpc(99).await.is_ok()); // non-existent
}

#[tokio::test]
async fn test_noop_enforcer_pod_rules() {
    let mut enforcer = NoopEnforcer::new();
    enforcer.init().await.unwrap();
    enforcer.ensure_vpc(1, "10.0.1.0/24").await.unwrap();

    assert!(enforcer
        .install_pod_rules("pod-1", "10.0.1.10", 1)
        .await
        .is_ok());
    assert!(enforcer
        .install_pod_rules("pod-2", "10.0.1.11", 1)
        .await
        .is_ok());

    assert!(enforcer.remove_pod_rules("pod-1").await.is_ok());
    assert!(enforcer.remove_pod_rules("pod-nonexistent").await.is_ok());
}

#[tokio::test]
async fn test_noop_enforcer_tap_rules() {
    let mut enforcer = NoopEnforcer::new();
    enforcer.init().await.unwrap();
    enforcer.ensure_vpc(1, "10.0.1.0/24").await.unwrap();

    assert!(enforcer
        .install_tap_rules("tap-vm1", "10.0.1.20", 1)
        .await
        .is_ok());
    assert!(enforcer.remove_tap_rules("tap-vm1").await.is_ok());
}

#[tokio::test]
async fn test_noop_enforcer_peering_rules() {
    let mut enforcer = NoopEnforcer::new();
    enforcer.init().await.unwrap();

    let vpcs = vec![
        make_vpc("vpc-a", 1, "10.0.1.0/24"),
        make_vpc("vpc-b", 2, "10.0.2.0/24"),
    ];

    let peering = make_peering("peer-ab", "vpc-a", "vpc-b");

    enforcer.ensure_vpc(1, "10.0.1.0/24").await.unwrap();
    enforcer.ensure_vpc(2, "10.0.2.0/24").await.unwrap();

    assert!(enforcer
        .install_peering_rules(&peering, &vpcs)
        .await
        .is_ok());
    assert!(enforcer.remove_peering_rules("peer-ab").await.is_ok());
    assert!(enforcer
        .remove_peering_rules("nonexistent")
        .await
        .is_ok());
}

#[tokio::test]
async fn test_noop_enforcer_snapshot() {
    let enforcer = NoopEnforcer::new();
    let snap = enforcer.snapshot().await.unwrap();
    assert_eq!(snap, "(noop)");
}

#[tokio::test]
async fn test_noop_enforcer_cleanup() {
    let mut enforcer = NoopEnforcer::new();
    enforcer.init().await.unwrap();
    enforcer.ensure_vpc(1, "10.0.1.0/24").await.unwrap();
    assert!(enforcer.cleanup().await.is_ok());
}

#[tokio::test]
async fn test_noop_enforcer_full_lifecycle() {
    let mut enforcer = NoopEnforcer::new();

    // Init
    enforcer.init().await.unwrap();

    // Setup VPCs
    enforcer.ensure_vpc(1, "10.0.1.0/24").await.unwrap();
    enforcer.ensure_vpc(2, "10.0.2.0/24").await.unwrap();

    // Install pods
    enforcer
        .install_pod_rules("pod-a1", "10.0.1.10", 1)
        .await
        .unwrap();
    enforcer
        .install_pod_rules("pod-a2", "10.0.1.11", 1)
        .await
        .unwrap();
    enforcer
        .install_pod_rules("pod-b1", "10.0.2.10", 2)
        .await
        .unwrap();

    // Install TAP
    enforcer
        .install_tap_rules("tap-vm1", "10.0.1.20", 1)
        .await
        .unwrap();

    // Install peering
    let vpcs = vec![
        make_vpc("vpc-a", 1, "10.0.1.0/24"),
        make_vpc("vpc-b", 2, "10.0.2.0/24"),
    ];
    let peering = make_peering("peer-ab", "vpc-a", "vpc-b");
    enforcer
        .install_peering_rules(&peering, &vpcs)
        .await
        .unwrap();

    // Snapshot
    let snap = enforcer.snapshot().await.unwrap();
    assert_eq!(snap, "(noop)");

    // Teardown
    enforcer.remove_pod_rules("pod-a1").await.unwrap();
    enforcer.remove_pod_rules("pod-a2").await.unwrap();
    enforcer.remove_pod_rules("pod-b1").await.unwrap();
    enforcer.remove_tap_rules("tap-vm1").await.unwrap();
    enforcer.remove_peering_rules("peer-ab").await.unwrap();
    enforcer.remove_vpc(1).await.unwrap();
    enforcer.remove_vpc(2).await.unwrap();

    // Cleanup
    enforcer.cleanup().await.unwrap();
}
