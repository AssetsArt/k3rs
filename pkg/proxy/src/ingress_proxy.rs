use async_trait::async_trait;
use pingora::prelude::*;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

use pkg_types::ingress::PathType;

/// A compiled Ingress routing rule for fast matching.
#[derive(Debug, Clone)]
pub struct IngressRouteRule {
    pub host: String,
    pub path: String,
    pub path_type: PathType,
    /// Backend address as "clusterIP:port"
    pub backend: String,
}

/// Pingora-based Ingress Controller — routes external HTTP traffic
/// to cluster services based on Ingress host/path rules.
pub struct IngressProxy {
    pub rules: Arc<RwLock<Vec<IngressRouteRule>>>,
    pub listen_port: u16,
}

/// Pingora `ProxyHttp` handler for Ingress routing.
struct IngressProxyHandler {
    rules: Arc<RwLock<Vec<IngressRouteRule>>>,
}

#[async_trait]
impl ProxyHttp for IngressProxyHandler {
    type CTX = ();

    fn new_ctx(&self) -> Self::CTX {}

    async fn upstream_peer(
        &self,
        session: &mut Session,
        _ctx: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>> {
        let host = session
            .req_header()
            .headers
            .get("host")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("")
            .to_string();

        // Strip port from host if present (e.g. "example.com:8080" → "example.com")
        let host_name = host.split(':').next().unwrap_or(&host);

        let path = session.req_header().uri.path().to_string();

        let rules = self.rules.read().await;

        // Find a matching rule (first match wins)
        for rule in rules.iter() {
            if rule.host != host_name {
                continue;
            }

            let matched = match rule.path_type {
                PathType::Exact => path == rule.path,
                PathType::Prefix => path.starts_with(&rule.path),
            };

            if matched {
                let peer = HttpPeer::new(&rule.backend, false, String::new());
                return Ok(Box::new(peer));
            }
        }

        // No matching ingress rule
        Err(pingora::Error::new(pingora::ErrorType::ConnectNoRoute))
    }
}

impl IngressProxy {
    /// Create a new ingress proxy on the given port.
    pub fn new(listen_port: u16) -> Self {
        Self {
            rules: Arc::new(RwLock::new(Vec::new())),
            listen_port,
        }
    }

    /// Rebuild the routing rules from Ingress + Service resources.
    pub async fn update_rules(
        &self,
        ingresses: &[pkg_types::ingress::Ingress],
        services: &[pkg_types::service::Service],
    ) {
        let mut new_rules = Vec::new();

        for ingress in ingresses {
            for rule in &ingress.spec.rules {
                for path in &rule.http.paths {
                    // Resolve the backend service to a ClusterIP:port
                    let backend = services
                        .iter()
                        .find(|s| {
                            s.name == path.backend.service_name && s.namespace == ingress.namespace
                        })
                        .and_then(|s| {
                            s.cluster_ip
                                .as_ref()
                                .map(|ip| format!("{}:{}", ip, path.backend.service_port))
                        });

                    if let Some(backend_addr) = backend {
                        new_rules.push(IngressRouteRule {
                            host: rule.host.clone(),
                            path: path.path.clone(),
                            path_type: path.path_type.clone(),
                            backend: backend_addr,
                        });
                    }
                }
            }
        }

        let count = new_rules.len();
        let mut rules = self.rules.write().await;
        *rules = new_rules;
        info!("IngressProxy rules updated: {} rules", count);
    }

    /// Start the Pingora-based ingress proxy in a background task.
    pub async fn start(&self) -> anyhow::Result<()> {
        info!(
            "Starting Pingora IngressProxy on 0.0.0.0:{}",
            self.listen_port
        );

        let mut server = Server::new(None)?;
        server.bootstrap();

        let handler = IngressProxyHandler {
            rules: self.rules.clone(),
        };

        let mut proxy = http_proxy_service(&server.configuration, handler);
        proxy.add_tcp(&format!("0.0.0.0:{}", self.listen_port));

        server.add_service(proxy);

        tokio::task::spawn_blocking(move || {
            server.run_forever();
        });

        info!("IngressProxy is running on port {}", self.listen_port);
        Ok(())
    }
}
