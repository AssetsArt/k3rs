use async_trait::async_trait;
use pingora::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::RwLock;
use tracing::info;

/// A routing table entry: maps `ClusterIP:port` to a list of backend pod addresses.
#[derive(Debug, Clone, Default)]
pub struct RoutingTable {
    /// Key: "clusterIP:port", Value: list of "podIP:targetPort"
    pub routes: HashMap<String, Vec<String>>,
}

/// Pingora-based Service Proxy — replaces kube-proxy on Agent nodes.
///
/// Listens for incoming connections and routes them to backend pods
/// based on the dynamic routing table populated from Service + Endpoint data.
pub struct ServiceProxy {
    pub routing_table: Arc<RwLock<RoutingTable>>,
    pub listen_port: u16,
    round_robin: Arc<AtomicUsize>,
}

/// The Pingora `ProxyHttp` handler for service proxying.
struct ServiceProxyHandler {
    routing_table: Arc<RwLock<RoutingTable>>,
    round_robin: Arc<AtomicUsize>,
}

#[async_trait]
impl ProxyHttp for ServiceProxyHandler {
    type CTX = ();

    fn new_ctx(&self) -> Self::CTX {}

    async fn upstream_peer(
        &self,
        session: &mut Session,
        _ctx: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>> {
        // Determine the target from the Host header or destination address
        let host = session
            .req_header()
            .headers
            .get("host")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("unknown")
            .to_string();

        let table = self.routing_table.read().await;

        // Try to match against the routing table
        if let Some(backends) = table.routes.get(&host) {
            if !backends.is_empty() {
                let idx = self.round_robin.fetch_add(1, Ordering::Relaxed) % backends.len();
                let backend = &backends[idx];
                let peer = HttpPeer::new(backend, false, String::new());
                return Ok(Box::new(peer));
            }
        }

        // Fallback: try without port matching (just plain host)
        for (key, backends) in table.routes.iter() {
            if key.starts_with(&host) && !backends.is_empty() {
                let idx = self.round_robin.fetch_add(1, Ordering::Relaxed) % backends.len();
                let backend = &backends[idx];
                let peer = HttpPeer::new(backend, false, String::new());
                return Ok(Box::new(peer));
            }
        }

        // No route found — return error
        Err(pingora::Error::new(pingora::ErrorType::ConnectNoRoute))
    }
}

impl ServiceProxy {
    /// Create a new service proxy listening on the given port.
    pub fn new(listen_port: u16) -> Self {
        Self {
            routing_table: Arc::new(RwLock::new(RoutingTable::default())),
            listen_port,
            round_robin: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Update the routing table from Service + Endpoint data.
    pub async fn update_routes(
        &self,
        services: &[pkg_types::service::Service],
        endpoints: &[pkg_types::endpoint::Endpoint],
    ) {
        let mut new_routes: HashMap<String, Vec<String>> = HashMap::new();

        for svc in services {
            let cluster_ip = match &svc.cluster_ip {
                Some(ip) => ip.clone(),
                None => continue,
            };

            // Find matching endpoints for this service
            let matching_eps: Vec<&pkg_types::endpoint::Endpoint> = endpoints
                .iter()
                .filter(|ep| ep.service_id == svc.id && ep.namespace == svc.namespace)
                .collect();

            for svc_port in &svc.spec.ports {
                let route_key = format!("{}:{}", cluster_ip, svc_port.port);
                let mut backends = Vec::new();

                for ep in &matching_eps {
                    for addr in &ep.addresses {
                        // Use the target_port from the service port spec
                        backends.push(format!("{}:{}", addr.ip, svc_port.target_port));
                    }
                }

                if !backends.is_empty() {
                    new_routes.insert(route_key, backends);
                }
            }
        }

        let route_count = new_routes.len();
        let mut table = self.routing_table.write().await;
        *table = RoutingTable { routes: new_routes };
        info!("ServiceProxy routing table updated: {} routes", route_count);
    }

    /// Start the Pingora-based service proxy in a background task.
    pub async fn start(&self) -> anyhow::Result<()> {
        info!(
            "Starting Pingora ServiceProxy on 0.0.0.0:{}",
            self.listen_port
        );

        let mut server = Server::new(None)?;
        server.bootstrap();

        let handler = ServiceProxyHandler {
            routing_table: self.routing_table.clone(),
            round_robin: self.round_robin.clone(),
        };

        let mut proxy = http_proxy_service(&server.configuration, handler);
        proxy.add_tcp(&format!("0.0.0.0:{}", self.listen_port));

        server.add_service(proxy);

        tokio::task::spawn_blocking(move || {
            server.run_forever();
        });

        info!("ServiceProxy is running on port {}", self.listen_port);
        Ok(())
    }
}
