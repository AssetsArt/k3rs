use chrono::Utc;
use pkg_state::client::StateStore;
use pkg_types::endpoint::{Endpoint, EndpointAddress, EndpointPort};
use pkg_types::pod::{Pod, PodStatus};
use pkg_types::service::Service;
use std::collections::HashMap;
use std::time::Duration;
use tracing::{info, warn};

/// Controller that auto-generates Endpoints from Running pods matched by
/// Service selectors. Uses Ghost IPv6 addresses when available.
pub struct EndpointController {
    store: StateStore,
    check_interval: Duration,
}

impl EndpointController {
    pub fn new(store: StateStore) -> Self {
        Self {
            store,
            check_interval: Duration::from_secs(pkg_constants::timings::ENDPOINT_CHECK_INTERVAL_SECS),
        }
    }

    pub fn start(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            info!(
                "EndpointController started (interval={}s)",
                self.check_interval.as_secs()
            );
            let mut event_rx = self.store.event_log.subscribe();
            let mut interval = tokio::time::interval(self.check_interval);
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if let Err(e) = self.reconcile().await {
                            warn!("EndpointController reconcile error: {}", e);
                        }
                    }
                    result = event_rx.recv() => {
                        match result {
                            Ok(ref event)
                                if event.key.starts_with("/registry/pods/")
                                    || event.key.starts_with("/registry/services/") =>
                            {
                                while event_rx.try_recv().is_ok() {}
                                if let Err(e) = self.reconcile().await {
                                    warn!("EndpointController reconcile error: {}", e);
                                }
                                while event_rx.try_recv().is_ok() {}
                                interval.reset();
                            }
                            Ok(_) => {}
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                if let Err(e) = self.reconcile().await {
                                    warn!("EndpointController reconcile error: {}", e);
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
        let services = self.load_all::<Service>("/registry/services/").await?;
        let pods = self.load_all::<Pod>("/registry/pods/").await?;

        for svc in &services {
            if svc.spec.selector.is_empty() {
                continue;
            }

            // Find Running pods matching this service's selector, namespace, and VPC
            let svc_vpc = svc.vpc.as_deref().unwrap_or("default");
            let matching_pods: Vec<&Pod> = pods
                .iter()
                .filter(|p| {
                    p.namespace == svc.namespace
                        && p.status == PodStatus::Running
                        && matches_selector(&p.labels, &svc.spec.selector)
                        && p.vpc_name
                            .as_deref()
                            .or(p.spec.vpc.as_deref())
                            .unwrap_or("default")
                            == svc_vpc
                })
                .collect();

            let addresses: Vec<EndpointAddress> = matching_pods
                .iter()
                .filter_map(|p| {
                    // Use Ghost IPv6 if available, fallback to K3RS_POD_IP-style guest IPv4
                    let ip = p.ghost_ipv6.clone()?;
                    Some(EndpointAddress {
                        ip,
                        node_name: p.node_name.clone(),
                        pod_id: Some(p.id.clone()),
                    })
                })
                .collect();

            let ports: Vec<EndpointPort> = svc
                .spec
                .ports
                .iter()
                .map(|sp| EndpointPort {
                    name: sp.name.clone(),
                    port: sp.target_port,
                    protocol: "TCP".to_string(),
                })
                .collect();

            let endpoint = Endpoint {
                id: svc.id.clone(),
                service_id: svc.id.clone(),
                service_name: svc.name.clone(),
                namespace: svc.namespace.clone(),
                addresses,
                ports,
                created_at: Utc::now(),
            };

            let key = format!("/registry/endpoints/{}/{}", svc.namespace, svc.id);
            let data = serde_json::to_vec(&endpoint)?;
            self.store.put(&key, &data).await?;
        }

        Ok(())
    }

    async fn load_all<T: serde::de::DeserializeOwned>(
        &self,
        prefix: &str,
    ) -> anyhow::Result<Vec<T>> {
        let entries = self.store.list_prefix(prefix).await?;
        Ok(entries
            .into_iter()
            .filter_map(|(_, v)| serde_json::from_slice(&v).ok())
            .collect())
    }
}

/// Check if pod labels match all entries in the service selector.
fn matches_selector(labels: &HashMap<String, String>, selector: &HashMap<String, String>) -> bool {
    selector
        .iter()
        .all(|(k, v)| labels.get(k).is_some_and(|lv| lv == v))
}
