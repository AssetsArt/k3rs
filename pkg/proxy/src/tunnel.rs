use async_trait::async_trait;
use pingora::prelude::*;
use std::sync::Arc;
use tracing::info;

/// A Pingora-based reverse tunnel proxy.
///
/// In the full architecture, agents run this proxy to tunnel all traffic
/// back to the control plane server. For Phase 1, it acts as a simple
/// HTTP reverse proxy that forwards requests to the configured upstream.
pub struct TunnelProxy {
    server_addr: String,
    listen_port: u16,
}

/// The Pingora `ProxyHttp` service handler.
struct TunnelService {
    upstream: Arc<String>,
}

#[async_trait]
impl ProxyHttp for TunnelService {
    type CTX = ();

    fn new_ctx(&self) -> Self::CTX {}

    async fn upstream_peer(
        &self,
        _session: &mut Session,
        _ctx: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>> {
        let peer = HttpPeer::new(&*self.upstream, false, String::new());
        Ok(Box::new(peer))
    }
}

impl TunnelProxy {
    /// Create a new tunnel proxy that listens on `listen_port` and
    /// forwards all HTTP traffic to `server_addr`.
    pub fn new(server_addr: &str, listen_port: u16) -> Self {
        Self {
            server_addr: server_addr.to_string(),
            listen_port,
        }
    }

    /// Start the Pingora proxy server in a background tokio task.
    pub async fn start(&self) -> anyhow::Result<()> {
        info!(
            "Starting Pingora tunnel proxy on 0.0.0.0:{} → {}",
            self.listen_port, self.server_addr
        );

        let mut server = Server::new(None)?;
        server.bootstrap();

        let service = TunnelService {
            upstream: Arc::new(self.server_addr.clone()),
        };

        let mut proxy = http_proxy_service(&server.configuration, service);
        proxy.add_tcp(&format!("0.0.0.0:{}", self.listen_port));

        server.add_service(proxy);

        // Run Pingora in a dedicated blocking thread since it takes over the thread
        let handle = tokio::task::spawn_blocking(move || {
            server.run_forever();
        });

        info!("Tunnel proxy is running");
        // We don't await the handle — it runs forever in the background
        drop(handle);

        Ok(())
    }
}

impl Default for TunnelProxy {
    fn default() -> Self {
        Self::new("127.0.0.1:6443", 6444)
    }
}
