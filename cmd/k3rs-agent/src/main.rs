mod api;
mod cache;
mod cli;
mod connectivity;
mod heartbeat;
mod loops;
mod recovery;
mod registration;
mod store;
#[cfg(test)]
mod tests;
mod vpc_client;

use cache::AgentStateCache;
use clap::Parser;
use connectivity::ConnectivityManager;
use pkg_network::dns::DnsServer;
use pkg_proxy::service_proxy::ServiceProxy;
use pkg_proxy::tunnel::TunnelProxy;
use pkg_types::config::{AgentConfigFile, load_config_file};
use pkg_types::node::NodeRegistrationRequest;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use store::AgentStore;
use tracing::{info, warn};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();

    // Initialize logging based on format
    match cli.log_format.as_str() {
        "json" => {
            tracing_subscriber::fmt()
                .json()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::from_default_env()
                        .add_directive(tracing::level_filters::LevelFilter::INFO.into())
                        .add_directive(tracing::level_filters::LevelFilter::ERROR.into())
                        .add_directive(tracing::level_filters::LevelFilter::WARN.into()),
                )
                .init();
        }
        _ => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::from_default_env()
                        .add_directive(tracing::level_filters::LevelFilter::INFO.into())
                        .add_directive(tracing::level_filters::LevelFilter::ERROR.into())
                        .add_directive(tracing::level_filters::LevelFilter::WARN.into()),
                )
                .init();
        }
    }

    // Load config file (returns defaults if file not found)
    let file_cfg: AgentConfigFile = load_config_file(&cli.config)?;
    info!("Config file: {}", cli.config);

    // Merge: CLI args > config file > defaults
    let server = cli
        .server
        .or(file_cfg.server)
        .unwrap_or_else(|| pkg_constants::network::DEFAULT_API_ADDR.to_string());
    let token = cli
        .token
        .or(file_cfg.token)
        .unwrap_or_else(|| pkg_constants::auth::DEFAULT_JOIN_TOKEN.to_string());
    let node_name = cli
        .node_name
        .or(file_cfg.node_name)
        .unwrap_or_else(|| "node-1".to_string());
    let proxy_port = cli
        .proxy_port
        .or(file_cfg.proxy_port)
        .unwrap_or(pkg_constants::network::DEFAULT_TUNNEL_PORT);
    let service_proxy_port = cli
        .service_proxy_port
        .or(file_cfg.service_proxy_port)
        .unwrap_or(pkg_constants::network::DEFAULT_SERVICE_PROXY_PORT);
    let dns_port = cli
        .dns_port
        .or(file_cfg.dns_port)
        .unwrap_or(pkg_constants::network::DEFAULT_DNS_PORT);

    info!("Starting k3rs-agent for node: {}", node_name);

    // =========================================================================
    // VPC client
    let vpc_client = Arc::new(vpc_client::VpcClient::new(cli.vpc_socket.clone()));
    info!("VPC client configured for socket: {}", cli.vpc_socket);

    // Phase A: Open AgentStore (SlateDB) and load cached state
    // =========================================================================
    let data_dir = cli.data_dir.clone();
    let store = match AgentStore::open(&data_dir).await {
        Ok(s) => s,
        Err(e) => {
            warn!(
                "Failed to open AgentStore at '{}', starting fresh: {}",
                data_dir, e
            );
            AgentStore::open("/tmp/k3rs-agent-fallback").await?
        }
    };

    let cached = match store.load().await {
        Ok(c) => c,
        Err(e) => {
            warn!("Failed to load from AgentStore, starting fresh: {}", e);
            None
        }
    };
    let has_cache = cached.is_some();

    let connectivity = Arc::new(ConnectivityManager::new());
    let cache = Arc::new(std::sync::RwLock::new(
        cached
            .clone()
            .unwrap_or_else(|| AgentStateCache::new(node_name.clone())),
    ));

    // =========================================================================
    // Phase B: Start services with stale data (before server contact)
    // =========================================================================

    // Start the Pingora Service Proxy
    let service_proxy = Arc::new(ServiceProxy::new(service_proxy_port));
    service_proxy.start().await?;

    // Pre-populate routes from cached services/endpoints if available.
    if let Some(ref c) = cached {
        service_proxy
            .update_routes(&c.services, &c.endpoints, &HashMap::new())
            .await;
        info!(
            "ServiceProxy pre-loaded {} cached services as routes",
            c.services.len()
        );
    }

    // Start the embedded DNS server.
    let dns_addr: SocketAddr = format!("0.0.0.0:{}", dns_port).parse()?;
    let dns_server = Arc::new(DnsServer::new(dns_addr));

    // Pre-populate DNS records from cached services if available.
    if let Some(ref c) = cached {
        dns_server.update_records(&c.services).await;
        info!(
            "DnsServer pre-loaded {} cached services as DNS records",
            c.services.len()
        );
    }

    // Start DNS listener in background
    {
        let ds = dns_server.clone();
        tokio::spawn(async move {
            if let Err(e) = ds.start().await {
                warn!("DNS server failed to start on port {}: {}", dns_port, e);
            }
        });
    }

    // Start the Pingora tunnel proxy in a background task.
    let server_host = server
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .to_string();
    tokio::spawn(async move {
        let proxy = TunnelProxy::new(&server_host, proxy_port);
        if let Err(e) = proxy.start().await {
            warn!("TunnelProxy failed to start: {}", e);
        }
    });

    // =========================================================================
    // Phase C: Attempt registration (non-fatal on failure)
    // =========================================================================
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()?;

    // Detect real machine resources to report as node capacity.
    let capacity = {
        use sysinfo::System;
        let mut sys = System::new_all();
        sys.refresh_all();
        let cpu_millis = (sys.cpus().len() as u64) * 1000;
        let memory_bytes = sys.total_memory();
        info!(
            "Detected machine capacity: {} vCPU ({} millicores), {:.1} GiB RAM",
            sys.cpus().len(),
            cpu_millis,
            memory_bytes as f64 / 1_073_741_824.0
        );
        pkg_types::pod::ResourceRequirements {
            cpu_millis,
            memory_bytes,
        }
    };

    // Query WireGuard public key from VPC daemon (best-effort)
    let (wg_public_key, wg_listen_port) = match vpc_client.get_wg_public_key().await {
        Ok((pk, port)) => {
            if let Some(ref key) = pk {
                info!(
                    "WireGuard public key from VPC daemon: {}...",
                    &key[..8.min(key.len())]
                );
            }
            (pk, port)
        }
        Err(e) => {
            warn!("Could not get WireGuard key from VPC daemon: {}", e);
            (None, None)
        }
    };

    let reg_req = NodeRegistrationRequest {
        token: token.clone(),
        node_name: node_name.clone(),
        address: "127.0.0.1".to_string(),
        labels: HashMap::new(),
        capacity: Some(capacity),
        wg_public_key,
        wg_listen_port,
    };

    info!("Connecting to server at {}", server);

    let cached_node_id = cache.read().unwrap().node_id.clone();
    match registration::try_connect(
        &client,
        &server,
        &token,
        &reg_req,
        &node_name,
        cached_node_id.as_deref(),
    )
    .await
    {
        Ok(new_identity) => {
            connectivity.set_connected();
            if let Some((node_id, api_port)) = new_identity {
                {
                    let mut c = cache.write().unwrap();
                    c.node_id = Some(node_id);
                    c.agent_api_port = Some(api_port);
                }
                let snapshot = cache.read().unwrap().clone();
                if let Err(e) = store.save(&snapshot).await {
                    warn!("Failed to save to AgentStore after registration: {}", e);
                }
            }
        }
        Err(e) => {
            if has_cache {
                let age = cache.read().unwrap().age_secs();
                warn!(
                    "Server unreachable: {}. Starting in offline mode, cache age: {}s",
                    e, age
                );
            } else {
                warn!(
                    "Server unreachable: {}. No cache available, waiting for server...",
                    e
                );
            }
            connectivity.set_offline();
        }
    }

    // =========================================================================
    // Phase D: Start heartbeat (connectivity-aware)
    // =========================================================================
    heartbeat::start_heartbeat_loop(
        server.clone(),
        node_name.clone(),
        token.clone(),
        connectivity.clone(),
        cache.clone(),
    );

    // =========================================================================
    // Phase E: Agent controller loops
    // =========================================================================
    loops::start_controller_loops(
        server.clone(),
        token.clone(),
        service_proxy.clone(),
        dns_server.clone(),
        cache.clone(),
        connectivity.clone(),
        reg_req.clone(),
        node_name.clone(),
        store.clone(),
        vpc_client.clone(),
    );

    // Block until Ctrl-C
    info!("Agent is running. Press Ctrl-C to stop.");
    tokio::signal::ctrl_c().await?;
    info!("Shutting down agent — flushing AgentStore WAL...");
    if let Err(e) = store.close().await {
        warn!("AgentStore close error: {}", e);
    }
    info!("Shutdown complete");

    Ok(())
}
