use chrono::Utc;
use pkg_state::client::StateStore;
use pkg_types::vpc::{Vpc, VpcStatus};
use std::time::Duration;
use tracing::{info, warn};

/// Controller that ensures the default VPC exists and reconciles VPC state.
pub struct VpcController {
    store: StateStore,
    check_interval: Duration,
}

impl VpcController {
    pub fn new(store: StateStore) -> Self {
        Self {
            store,
            check_interval: Duration::from_secs(15),
        }
    }

    pub fn start(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            info!(
                "VpcController started (interval={}s)",
                self.check_interval.as_secs()
            );
            let mut event_rx = self.store.event_log.subscribe();
            let mut interval = tokio::time::interval(self.check_interval);
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if let Err(e) = self.reconcile().await {
                            warn!("VpcController reconcile error: {}", e);
                        }
                    }
                    result = event_rx.recv() => {
                        match result {
                            Ok(ref event)
                                if event.key.starts_with("/registry/vpcs/") =>
                            {
                                while event_rx.try_recv().is_ok() {}
                                if let Err(e) = self.reconcile().await {
                                    warn!("VpcController reconcile error: {}", e);
                                }
                                while event_rx.try_recv().is_ok() {}
                                interval.reset();
                            }
                            Ok(_) => {}
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                if let Err(e) = self.reconcile().await {
                                    warn!("VpcController reconcile error: {}", e);
                                }
                                interval.reset();
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                }
            }
        })
    }

    async fn reconcile(&self) -> anyhow::Result<()> {
        self.ensure_default_vpc().await?;
        Ok(())
    }

    /// Ensure the default VPC exists (name: "default", vpc_id: 1, ipv4_cidr: "10.42.0.0/16").
    async fn ensure_default_vpc(&self) -> anyhow::Result<()> {
        let key = "/registry/vpcs/default";
        if self.store.get(key).await?.is_none() {
            let vpc = Vpc {
                name: "default".to_string(),
                vpc_id: 1,
                ipv4_cidr: "10.42.0.0/16".to_string(),
                status: VpcStatus::Active,
                created_at: Utc::now(),
            };
            let data = serde_json::to_vec(&vpc)?;
            self.store.put(key, &data).await?;
            info!("Seeded default VPC (vpc_id=1, cidr=10.42.0.0/16)");
        }
        Ok(())
    }
}
