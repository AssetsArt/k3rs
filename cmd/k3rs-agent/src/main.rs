mod api;
mod cache;
mod connectivity;
mod store;
mod vpc_client;
#[cfg(test)]
mod tests;

use cache::AgentStateCache;
use chrono::Utc;
use clap::Parser;
use connectivity::ConnectivityManager;
use pkg_container::ContainerRuntime;
use pkg_network::dns::DnsServer;
use pkg_proxy::service_proxy::ServiceProxy;
use pkg_proxy::tunnel::TunnelProxy;
use pkg_types::config::{AgentConfigFile, load_config_file};
use pkg_types::node::{NodeRegistrationRequest, NodeRegistrationResponse};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use store::AgentStore;
use tracing::{error, info, warn};

#[derive(Parser, Debug)]
#[command(name = "k3rs-agent", about = "k3rs node agent (data plane)")]
struct Cli {
    /// Path to YAML config file
    #[arg(long, short, default_value_t = format!("{}/agent-config.yaml", pkg_constants::paths::CONFIG_DIR))]
    config: String,

    /// Server API endpoint
    #[arg(long)]
    server: Option<String>,

    /// Join token for registration
    #[arg(long)]
    token: Option<String>,

    /// Node name
    #[arg(long)]
    node_name: Option<String>,

    /// Local port for the tunnel proxy
    #[arg(long)]
    proxy_port: Option<u16>,

    /// Local port for the service proxy
    #[arg(long)]
    service_proxy_port: Option<u16>,

    /// Local port for the embedded DNS server
    #[arg(long)]
    dns_port: Option<u16>,

    /// Log format: 'text' or 'json'
    #[arg(long, default_value = "text")]
    log_format: String,

    /// Path to the local data directory (AgentStore / SlateDB location)
    #[arg(long, default_value_t = pkg_constants::paths::DATA_DIR.to_string())]
    data_dir: String,

    /// Path to the VPC daemon Unix socket
    #[arg(long, default_value_t = pkg_constants::paths::VPC_SOCKET.to_string())]
    vpc_socket: String,
}

/// Attempt registration with the server. Returns (node_id, agent_api_port, response) on success.
async fn try_register(
    client: &reqwest::Client,
    server: &str,
    req: &NodeRegistrationRequest,
    node_name: &str,
) -> anyhow::Result<(String, u16, NodeRegistrationResponse)> {
    let url = format!("{}/register", server.trim_end_matches('/'));
    let resp = client.post(&url).json(req).send().await?;
    if resp.status().is_success() {
        let reg_resp: NodeRegistrationResponse = resp.json().await?;
        let node_id = reg_resp.node_id.clone();
        let port = reg_resp.agent_api_port;
        info!(
            "Successfully registered as node_id={}, assigned API port {}",
            node_id, port
        );

        // Store certs to disk for future mTLS connections
        let cert_dir = format!("{}/certs/{}", pkg_constants::paths::CONFIG_DIR, node_name);
        tokio::fs::create_dir_all(&cert_dir).await?;
        tokio::fs::write(format!("{}/node.crt", cert_dir), &reg_resp.certificate).await?;
        tokio::fs::write(format!("{}/node.key", cert_dir), &reg_resp.private_key).await?;
        tokio::fs::write(format!("{}/ca.crt", cert_dir), &reg_resp.server_ca).await?;
        info!("Certificates saved to {}", cert_dir);

        Ok((node_id, port, reg_resp))
    } else {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        Err(anyhow::anyhow!(
            "Registration failed: {} - {}",
            status,
            body
        ))
    }
}

/// Try to connect to the server. First attempts a heartbeat probe (if we have
/// a cached node_id). If the server still knows us, skip registration entirely.
/// Otherwise fall back to full registration.
///
/// Returns `Ok(true)` if we're now connected (either via probe or registration),
/// `Ok(false)` if we had a cached probe success (node_id already set),
/// `Err` if both probe and registration failed.
async fn try_connect(
    client: &reqwest::Client,
    server: &str,
    token: &str,
    reg_req: &NodeRegistrationRequest,
    node_name: &str,
    cached_node_id: Option<&str>,
) -> anyhow::Result<Option<(String, u16)>> {
    // If we have a cached node_id, probe the server with a heartbeat first.
    // This avoids re-registration when the server already knows this node
    // (e.g. agent restart while server is still running).
    if let Some(node_id) = cached_node_id {
        let probe_url = format!(
            "{}/api/v1/nodes/{}/heartbeat",
            server.trim_end_matches('/'),
            node_name
        );
        match client
            .put(&probe_url)
            .header("Authorization", format!("Bearer {}", token))
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                info!(
                    "Heartbeat probe succeeded — server still knows node_id={}, skipping registration",
                    node_id
                );
                return Ok(None); // Already known, no new node_id/port needed
            }
            Ok(resp) => {
                info!(
                    "Heartbeat probe returned {} — will re-register",
                    resp.status()
                );
            }
            Err(e) => {
                info!("Heartbeat probe failed: {} — will try registration", e);
            }
        }
    }

    // Probe failed or no cached node_id — do full registration
    let (node_id, port, _resp) = try_register(client, server, reg_req, node_name).await?;
    Ok(Some((node_id, port)))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Initialize logging based on format
    match cli.log_format.as_str() {
        "json" => {
            tracing_subscriber::fmt()
                .json()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::from_default_env()
                        .add_directive(tracing::level_filters::LevelFilter::INFO.into()),
                )
                .init();
        }
        _ => {
            tracing_subscriber::fmt::init();
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
            // Re-open at a temp path so the rest of the code still has a store.
            // In practice this should never happen on a well-formed filesystem.
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
    // Uses the same update_routes() path as the live sync loop — no file I/O.
    if let Some(ref c) = cached {
        service_proxy.update_routes(&c.services, &c.endpoints).await;
        info!(
            "ServiceProxy pre-loaded {} cached services as routes",
            c.services.len()
        );
    }

    // Start the embedded DNS server.
    // UdpSocket::bind(0.0.0.0:5353) on macOS can hang because mDNSResponder
    // holds the port. We pre-populate the in-memory record table first (no
    // socket needed), then start the listener in a background task.
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

    // Start DNS listener in background — never block main startup on port bind
    {
        let ds = dns_server.clone();
        tokio::spawn(async move {
            if let Err(e) = ds.start().await {
                warn!("DNS server failed to start on port {}: {}", dns_port, e);
            }
        });
    }

    // Start the Pingora tunnel proxy in a background task.
    // Pingora's Server::new() + bootstrap() can block when a second Pingora
    // server is created while the first one is already running, so we must
    // not await it on the main flow.
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

    let reg_req = NodeRegistrationRequest {
        token: token.clone(),
        node_name: node_name.clone(),
        address: "127.0.0.1".to_string(),
        labels: HashMap::new(),
        capacity: Some(capacity),
    };

    info!("Connecting to server at {}", server);

    let cached_node_id = cache.read().unwrap().node_id.clone();
    match try_connect(
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
            // Reconnect loop in controller thread handles retry (see Phase E)
        }
    }

    // =========================================================================
    // Phase D: Start heartbeat (connectivity-aware)
    // =========================================================================
    let server_base = server.clone();
    let heartbeat_node_name = node_name.clone();
    let heartbeat_token = token.clone();
    let heartbeat_conn = connectivity.clone();
    let heartbeat_cache = cache.clone();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("heartbeat runtime");
        rt.block_on(async move {
            let client = reqwest::Client::new();
            let mut fail_count = 0u32;
            loop {
                // Connected: poll every 10s. Failing: exponential backoff 1s→2s→4s→30s.
                //
                // `fail_count` is incremented *after* each failure, so it is
                // 1-based at the top of the loop. We subtract 1 to convert to
                // the 0-based index that `backoff_duration` expects, ensuring
                // the first retry fires after 1s (not 2s).
                let delay = if fail_count == 0 {
                    std::time::Duration::from_secs(10)
                } else {
                    ConnectivityManager::backoff_duration(fail_count.saturating_sub(1))
                };
                tokio::time::sleep(delay).await;

                // Skip heartbeat if we have no node_id yet (never registered)
                let has_node_id = heartbeat_cache.read().unwrap().node_id.is_some();
                if !has_node_id {
                    continue;
                }

                let url = format!(
                    "{}/api/v1/nodes/{}/heartbeat",
                    server_base.trim_end_matches('/'),
                    heartbeat_node_name
                );
                match client
                    .put(&url)
                    .header("Authorization", format!("Bearer {}", heartbeat_token))
                    .timeout(std::time::Duration::from_secs(5))
                    .send()
                    .await
                {
                    Ok(resp) if resp.status().is_success() => {
                        if fail_count > 0 {
                            info!("Heartbeat recovered after {} failures", fail_count);
                        }
                        fail_count = 0;
                        heartbeat_conn.set_connected();
                        info!("Heartbeat OK for {} (status=200)", heartbeat_node_name,);
                    }
                    Ok(resp) => {
                        fail_count += 1;
                        warn!(
                            "Heartbeat failed for {} (status={})",
                            heartbeat_node_name,
                            resp.status()
                        );
                        let age = heartbeat_cache.read().unwrap().age_secs();
                        heartbeat_conn.set_reconnecting(fail_count);
                        warn!(
                            "Server unreachable, retrying (attempt {}, cache age: {}s)",
                            fail_count, age
                        );
                    }
                    Err(e) => {
                        fail_count += 1;
                        warn!("Heartbeat failed for {}: {}", heartbeat_node_name, e);
                        let age = heartbeat_cache.read().unwrap().age_secs();
                        heartbeat_conn.set_reconnecting(fail_count);
                        warn!(
                            "Server unreachable, retrying (attempt {}, cache age: {}s)",
                            fail_count, age
                        );
                    }
                }
            }
        });
    });
    info!("Heartbeat loop started");

    // =========================================================================
    // Phase E: Agent controller loops
    // =========================================================================
    info!("Starting node controllers (pod-sync, image-report, route-sync)");

    let ctrl_server = server.clone();
    let ctrl_token = token.clone();
    let ctrl_service_proxy = service_proxy.clone();
    let ctrl_dns_server = dns_server.clone();
    let ctrl_cache = cache.clone();
    let ctrl_conn = connectivity.clone();
    let ctrl_reg_req = reg_req.clone();
    let ctrl_node_name = node_name.clone();
    let ctrl_store = store.clone();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("controller runtime");

        rt.block_on(async move {
            let client = reqwest::Client::builder()
                .danger_accept_invalid_certs(true)
                .build()
                .unwrap();

            // Read node_id and agent_api_port from cache (may be None if never registered)
            let (initial_node_id, initial_api_port) = {
                let c = ctrl_cache.read().unwrap();
                (c.node_id.clone(), c.agent_api_port)
            };

            // Init container runtime (may download youki/crun)
            let runtime: Option<Arc<ContainerRuntime>> = match ContainerRuntime::new(None::<&str>)
                .await
            {
                Ok(rt) => {
                    let rt_arc = Arc::new(rt);
                    info!("Container runtime ready: {}", rt_arc.backend_name());

                    // Start Agent API server for exec/logs (if we know the port)
                    if let Some(api_port) = initial_api_port {
                        let agent_state = api::AgentState {
                            runtime: rt_arc.clone(),
                        };
                        let agent_router = api::create_agent_router(agent_state);
                        let listener =
                            tokio::net::TcpListener::bind(format!("0.0.0.0:{}", api_port))
                                .await
                                .expect("Failed to bind agent API port");
                        info!("Agent API listening on 0.0.0.0:{}", api_port);
                        tokio::spawn(async move {
                            axum::serve(listener, agent_router).await.ok();
                        });
                    }

                    // --- Agent Recovery ---
                    info!("Starting agent recovery procedure...");
                    let discovered = rt_arc
                        .discover_running_containers()
                        .await
                        .unwrap_or_default();

                    if initial_node_id.is_some() {
                        let url = format!(
                            "{}/api/v1/pods?fieldSelector=spec.nodeName={}",
                            ctrl_server.trim_end_matches('/'),
                            ctrl_node_name
                        );
                        let desired_pods: Vec<pkg_types::pod::Pod> = match client
                            .get(&url)
                            .header("Authorization", format!("Bearer {}", ctrl_token))
                            .send()
                            .await
                        {
                            Ok(resp) => resp.json().await.unwrap_or_default(),
                            Err(e) => {
                                warn!("Agent recovery: failed to fetch desired pods: {}", e);
                                // Use cached pods if server unreachable
                                ctrl_cache.read().unwrap().pods.clone()
                            }
                        };

                        let mut desired_running_ids = std::collections::HashMap::new();
                        for pod in &desired_pods {
                            desired_running_ids
                                .insert(pod.id.clone(), (pod.name.clone(), pod.namespace.clone()));
                        }

                        for cid in discovered {
                            if let Some((pod_name, pod_ns)) = desired_running_ids.get(&cid) {
                                info!("Agent recovery: adopting desired container {}", cid);
                                let status_url = format!(
                                    "{}/api/v1/namespaces/{}/pods/{}/status",
                                    ctrl_server.trim_end_matches('/'),
                                    pod_ns,
                                    pod_name
                                );
                                let _ = client
                                    .put(&status_url)
                                    .header("Authorization", format!("Bearer {}", ctrl_token))
                                    .json(&pkg_types::pod::PodStatus::Running)
                                    .send()
                                    .await;
                            } else {
                                info!("Agent recovery: stopping orphaned container {}", cid);
                                let _ = rt_arc.cleanup_container(&cid).await;
                            }
                        }
                    } else {
                        info!("Agent recovery: no node_id, skipping pod reconciliation");
                        // Still cleanup any orphaned containers
                        for cid in discovered {
                            info!("Agent recovery: stopping orphaned container {}", cid);
                            let _ = rt_arc.cleanup_container(&cid).await;
                        }
                    }
                    info!("Agent recovery complete.");

                    Some(rt_arc)
                }
                Err(e) => {
                    warn!("Container runtime not available: {}. Pods will fail.", e);
                    None
                }
            };

            // Image report loop
            let img_runtime = runtime.clone();
            let img_client = client.clone();
            let img_server = ctrl_server.clone();
            let img_token = ctrl_token.clone();
            let img_cache = ctrl_cache.clone();
            let img_conn = ctrl_conn.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
                loop {
                    interval.tick().await;

                    // Skip when not connected
                    if !img_conn.is_connected() {
                        continue;
                    }

                    let node_id = img_cache.read().unwrap().node_id.clone();
                    let Some(ref node_id) = node_id else {
                        continue;
                    };

                    let Some(ref rt) = img_runtime else {
                        continue;
                    };
                    match rt.list_images().await {
                        Ok(images) => {
                            let url = format!(
                                "{}/api/v1/nodes/{}/images",
                                img_server.trim_end_matches('/'),
                                node_id
                            );
                            let _ = img_client
                                .put(&url)
                                .header("Authorization", format!("Bearer {}", img_token))
                                .json(&images)
                                .send()
                                .await;
                        }
                        Err(e) => warn!("Failed to list images: {}", e),
                    }
                }
            });

            // Reconnect loop — probes and re-registers with the server when not
            // connected. Handles initial OFFLINE, runtime RECONNECTING, server
            // restart, and leader election scenarios.
            let rc_client = client.clone();
            let rc_server = ctrl_server.clone();
            let rc_token = ctrl_token.clone();
            let rc_reg_req = ctrl_reg_req.clone();
            let rc_node_name = ctrl_node_name.clone();
            let rc_cache = ctrl_cache.clone();
            let rc_conn = ctrl_conn.clone();
            let rc_store = ctrl_store.clone();
            tokio::spawn(async move {
                let mut attempt = 0u32;
                loop {
                    // When connected, idle and reset attempt counter
                    if rc_conn.is_connected() {
                        attempt = 0;
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        continue;
                    }

                    let delay = ConnectivityManager::backoff_duration(attempt);
                    tokio::time::sleep(delay).await;

                    let cached_node_id = rc_cache.read().unwrap().node_id.clone();
                    match try_connect(
                        &rc_client,
                        &rc_server,
                        &rc_token,
                        &rc_reg_req,
                        &rc_node_name,
                        cached_node_id.as_deref(),
                    )
                    .await
                    {
                        Ok(new_identity) => {
                            info!("Reconnected to server after {} attempts", attempt + 1);
                            if let Some((node_id, api_port)) = new_identity {
                                {
                                    let mut c = rc_cache.write().unwrap();
                                    c.node_id = Some(node_id);
                                    c.agent_api_port = Some(api_port);
                                }
                                let snapshot = rc_cache.read().unwrap().clone();
                                if let Err(e) = rc_store.save(&snapshot).await {
                                    warn!("Failed to save to AgentStore after reconnect: {}", e);
                                }
                            }
                            rc_conn.set_connected();
                            attempt = 0;
                        }
                        Err(e) => {
                            attempt += 1;
                            let age = rc_cache.read().unwrap().age_secs();
                            warn!(
                                "Reconnect failed (attempt {}, cache age: {}s): {}",
                                attempt, age, e
                            );
                        }
                    }
                }
            });

            // Pod sync loop
            let sync_client = client.clone();
            let sync_server = ctrl_server.clone();
            let sync_token = ctrl_token.clone();
            let sync_node_name = ctrl_node_name.clone();
            let sync_cache = ctrl_cache.clone();
            let sync_conn = ctrl_conn.clone();
            let sync_store = ctrl_store.clone();
            let sync_vpc = vpc_client.clone();
            let in_flight: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>> =
                std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(5));
                loop {
                    interval.tick().await;

                    // Skip when not connected — do not create containers from stale cache
                    if !sync_conn.is_connected() {
                        continue;
                    }

                    // Pod sync: skip if not registered yet
                    let has_registration = sync_cache.read().unwrap().node_id.is_some();
                    if !has_registration {
                        continue;
                    };

                    let url = format!(
                        "{}/api/v1/pods?fieldSelector=spec.nodeName={}",
                        sync_server.trim_end_matches('/'),
                        sync_node_name
                    );

                    let resp = match sync_client
                        .get(&url)
                        .header("Authorization", format!("Bearer {}", sync_token))
                        .send()
                        .await
                    {
                        Ok(r) => r,
                        Err(e) => {
                            warn!("Pod sync failed to fetch pods: {}", e);
                            continue;
                        }
                    };

                    match resp.json::<Vec<pkg_types::pod::Pod>>().await {
                        Ok(pods) => {
                            // Update in-memory cache with fetched pods (outside lock for save)
                            {
                                let mut c = sync_cache.write().unwrap();
                                c.pods = pods.clone();
                                c.last_synced_at = Utc::now();
                            }
                            let snapshot = sync_cache.read().unwrap().clone();
                            if let Err(e) = sync_store.save(&snapshot).await {
                                warn!("Failed to save to AgentStore after pod sync: {}", e);
                            }

                            // --- Health monitoring: check Running pods ---
                            if let Some(ref runtime) = runtime {
                                for pod in pods
                                    .iter()
                                    .filter(|p| p.status == pkg_types::pod::PodStatus::Running)
                                {
                                    match runtime.container_state(&pod.id).await {
                                        Ok(state)
                                            if state.status == "stopped"
                                                || state.status == "exited" =>
                                        {
                                            warn!(
                                                "[pod:{}] Container stopped unexpectedly",
                                                pod.name
                                            );

                                            // Release VPC allocation (best-effort)
                                            let vpc_name = pod.vpc_name.as_deref()
                                                .or(pod.spec.vpc.as_deref())
                                                .unwrap_or("default");
                                            if let Err(e) = sync_vpc.release(&pod.id, vpc_name).await {
                                                warn!("[pod:{}] VPC release failed: {}", pod.name, e);
                                            }

                                            if let Ok(logs) =
                                                runtime.container_logs(&pod.id, 20).await
                                            {
                                                for line in logs {
                                                    warn!("[pod:{}]   > {}", pod.name, line);
                                                }
                                            }

                                            let status_url = format!(
                                                "{}/api/v1/namespaces/{}/pods/{}/status",
                                                sync_server.trim_end_matches('/'),
                                                pod.namespace,
                                                pod.name
                                            );
                                            let _ = sync_client
                                                .put(&status_url)
                                                .header(
                                                    "Authorization",
                                                    format!("Bearer {}", sync_token),
                                                )
                                                .json(&pkg_types::pod::PodStatus::Failed)
                                                .send()
                                                .await;
                                        }
                                        Err(_) => {
                                            warn!(
                                                "[pod:{}] Container not found in runtime",
                                                pod.name
                                            );

                                            // Release VPC allocation (best-effort)
                                            let vpc_name = pod.vpc_name.as_deref()
                                                .or(pod.spec.vpc.as_deref())
                                                .unwrap_or("default");
                                            if let Err(e) = sync_vpc.release(&pod.id, vpc_name).await {
                                                warn!("[pod:{}] VPC release failed: {}", pod.name, e);
                                            }

                                            let status_url = format!(
                                                "{}/api/v1/namespaces/{}/pods/{}/status",
                                                sync_server.trim_end_matches('/'),
                                                pod.namespace,
                                                pod.name
                                            );
                                            let _ = sync_client
                                                .put(&status_url)
                                                .header(
                                                    "Authorization",
                                                    format!("Bearer {}", sync_token),
                                                )
                                                .json(&pkg_types::pod::PodStatus::Failed)
                                                .send()
                                                .await;
                                        }
                                        _ => {} // Still running, all good
                                    }
                                }
                            }

                            // --- Schedule new pods ---
                            for pod in pods {
                                if pod.status == pkg_types::pod::PodStatus::Scheduled
                                    || pod.status == pkg_types::pod::PodStatus::ContainerCreating
                                {
                                    info!(
                                        "Found scheduled pod: {} (image: {})",
                                        pod.name,
                                        pod.spec
                                            .containers
                                            .first()
                                            .map(|c| c.image.as_str())
                                            .unwrap_or("unknown")
                                    );

                                    let Some(ref rt_arc) = runtime else {
                                        error!(
                                            "[pod:{}] No container runtime available!",
                                            pod.name
                                        );
                                        let status_url = format!(
                                            "{}/api/v1/namespaces/{}/pods/{}/status",
                                            sync_server.trim_end_matches('/'),
                                            pod.namespace,
                                            pod.name
                                        );
                                        let _ = sync_client
                                            .put(&status_url)
                                            .header(
                                                "Authorization",
                                                format!("Bearer {}", sync_token),
                                            )
                                            .json(&pkg_types::pod::PodStatus::Failed)
                                            .send()
                                            .await;
                                        continue;
                                    };

                                    let pod_runtime = rt_arc.clone();
                                    let pod_client = sync_client.clone();
                                    let pod_server = sync_server.clone();
                                    let pod_token = sync_token.clone();
                                    let pod_in_flight = in_flight.clone();
                                    let pod_vpc = sync_vpc.clone();

                                    {
                                        let mut set = pod_in_flight.lock().unwrap();
                                        if set.contains(&pod.id) {
                                            continue;
                                        }
                                        set.insert(pod.id.clone());
                                    }

                                    tokio::spawn(async move {
                                        let status_url = format!(
                                            "{}/api/v1/namespaces/{}/pods/{}/status",
                                            pod_server.trim_end_matches('/'),
                                            pod.namespace,
                                            pod.name
                                        );

                                        let container_spec = pod.spec.containers.first();
                                        let image = container_spec
                                            .map(|c| c.image.clone())
                                            .unwrap_or_else(|| "alpine:latest".to_string());
                                        let command: Vec<String> = container_spec
                                            .map(|c| {
                                                let mut cmd = c.command.clone();
                                                cmd.extend(c.args.clone());
                                                cmd
                                            })
                                            .unwrap_or_default();
                                        let env = container_spec
                                            .map(|c| c.env.clone())
                                            .unwrap_or_default();

                                        // 0. Allocate VPC address
                                        let vpc_name = pod.spec.vpc.as_deref().unwrap_or("default");
                                        let vpc_alloc = match pod_vpc.allocate(&pod.id, vpc_name).await {
                                            Ok(alloc) => {
                                                info!(
                                                    "[pod:{}] VPC allocated: ghost_ipv6={}, guest_ipv4={}, vpc_id={}",
                                                    pod.name, alloc.1, alloc.0, alloc.2
                                                );
                                                Some(alloc)
                                            }
                                            Err(e) => {
                                                error!("[pod:{}] VPC allocation failed: {}", pod.name, e);
                                                let _ = pod_client
                                                    .put(&status_url)
                                                    .header(
                                                        "Authorization",
                                                        format!("Bearer {}", pod_token),
                                                    )
                                                    .json(&pkg_types::pod::PodStatus::Failed)
                                                    .send()
                                                    .await;
                                                pod_in_flight.lock().unwrap().remove(&pod.id);
                                                return;
                                            }
                                        };

                                        // 1. Pull Image
                                        info!("[pod:{}] Pulling image: {}", pod.name, image);
                                        if let Err(e) = pod_runtime.pull_image(&image).await {
                                            error!("[pod:{}] Image pull failed: {}", pod.name, e);
                                            let _ = pod_client
                                                .put(&status_url)
                                                .header(
                                                    "Authorization",
                                                    format!("Bearer {}", pod_token),
                                                )
                                                .json(&pkg_types::pod::PodStatus::Failed)
                                                .send()
                                                .await;
                                            return;
                                        }

                                        // 2. Create Container
                                        info!("[pod:{}] Creating container: {}", pod.name, pod.id);
                                        if let Err(e) = pod_runtime
                                            .create_container(
                                                &pod.id,
                                                &image,
                                                &command,
                                                &env,
                                                pod.spec.runtime.as_deref(),
                                            )
                                            .await
                                        {
                                            error!(
                                                "[pod:{}] Container creation failed: {}",
                                                pod.name, e
                                            );
                                            let _ = pod_client
                                                .put(&status_url)
                                                .header(
                                                    "Authorization",
                                                    format!("Bearer {}", pod_token),
                                                )
                                                .json(&pkg_types::pod::PodStatus::Failed)
                                                .send()
                                                .await;
                                            return;
                                        }

                                        // 3. Start Container
                                        info!("[pod:{}] Starting container: {}", pod.name, pod.id);
                                        if let Err(e) = pod_runtime.start_container(&pod.id).await {
                                            error!(
                                                "[pod:{}] Container start failed: {}",
                                                pod.name, e
                                            );
                                            pod_in_flight.lock().unwrap().remove(&pod.id);
                                            let _ = pod_runtime.cleanup_container(&pod.id).await;
                                            let _ = pod_client
                                                .put(&status_url)
                                                .header(
                                                    "Authorization",
                                                    format!("Bearer {}", pod_token),
                                                )
                                                .json(&pkg_types::pod::PodStatus::Failed)
                                                .send()
                                                .await;
                                            return;
                                        }

                                        // 4. Success
                                        pod_in_flight.lock().unwrap().remove(&pod.id);
                                        info!(
                                            "[pod:{}] Container running via {}",
                                            pod.name,
                                            pod_runtime.backend_name_for(&pod.id)
                                        );
                                        let _ = pod_client
                                            .put(&status_url)
                                            .header(
                                                "Authorization",
                                                format!("Bearer {}", pod_token),
                                            )
                                            .json(&pkg_types::pod::PodStatus::Running)
                                            .send()
                                            .await;

                                        // 5. Report VPC info to server (best-effort)
                                        if let Some((_, ref ghost_ipv6, _)) = vpc_alloc {
                                            let vpc_url = format!(
                                                "{}/api/v1/namespaces/{}/pods/{}/vpc",
                                                pod_server.trim_end_matches('/'),
                                                pod.namespace,
                                                pod.name
                                            );
                                            let vpc_body = serde_json::json!({
                                                "ghost_ipv6": ghost_ipv6,
                                                "vpc_name": vpc_name,
                                            });
                                            if let Err(e) = pod_client
                                                .put(&vpc_url)
                                                .header(
                                                    "Authorization",
                                                    format!("Bearer {}", pod_token),
                                                )
                                                .json(&vpc_body)
                                                .send()
                                                .await
                                            {
                                                warn!("[pod:{}] Failed to report VPC info: {}", pod.name, e);
                                            }
                                        }
                                    });
                                }
                            }
                        }
                        Err(e) => {
                            warn!("Failed to parse pods from JSON: {}", e);
                        }
                    }
                }
            });

            // Route sync loop
            let route_client = client.clone();
            let route_server = ctrl_server.clone();
            let route_token = ctrl_token.clone();
            let route_cache = ctrl_cache.clone();
            let route_conn = ctrl_conn.clone();
            let route_store = ctrl_store.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(10));
                loop {
                    interval.tick().await;

                    // Skip when not connected
                    if !route_conn.is_connected() {
                        continue;
                    }

                    let base = route_server.trim_end_matches('/');
                    let auth = format!("Bearer {}", route_token);

                    let namespaces: Vec<pkg_types::namespace::Namespace> = match route_client
                        .get(format!("{}/api/v1/namespaces", base))
                        .header("Authorization", &auth)
                        .send()
                        .await
                    {
                        Ok(r) => r.json().await.unwrap_or_default(),
                        Err(e) => {
                            warn!("Route sync: failed to fetch namespaces: {}", e);
                            continue;
                        }
                    };

                    let ns_names: Vec<String> = if namespaces.is_empty() {
                        vec!["default".to_string()]
                    } else {
                        namespaces.iter().map(|n| n.name.clone()).collect()
                    };

                    let mut all_services = Vec::new();
                    let mut all_endpoints = Vec::new();

                    for ns in &ns_names {
                        let services: Vec<pkg_types::service::Service> = match route_client
                            .get(format!("{}/api/v1/namespaces/{}/services", base, ns))
                            .header("Authorization", &auth)
                            .send()
                            .await
                        {
                            Ok(r) => r.json().await.unwrap_or_default(),
                            Err(e) => {
                                warn!("Route sync: failed to fetch services for ns {}: {}", ns, e);
                                continue;
                            }
                        };

                        let endpoints: Vec<pkg_types::endpoint::Endpoint> = match route_client
                            .get(format!("{}/api/v1/namespaces/{}/endpoints", base, ns))
                            .header("Authorization", &auth)
                            .send()
                            .await
                        {
                            Ok(r) => r.json().await.unwrap_or_default(),
                            Err(e) => {
                                warn!("Route sync: failed to fetch endpoints for ns {}: {}", ns, e);
                                continue;
                            }
                        };

                        all_services.extend(services);
                        all_endpoints.extend(endpoints);
                    }

                    // Update in-memory routing + DNS (live, in-memory)
                    ctrl_service_proxy
                        .update_routes(&all_services, &all_endpoints)
                        .await;
                    ctrl_dns_server.update_records(&all_services).await;

                    // Persist to AgentStore (single WriteBatch: meta + services +
                    // endpoints + derived /agent/routes + /agent/dns-records)
                    {
                        let mut c = route_cache.write().unwrap();
                        c.services = all_services;
                        c.endpoints = all_endpoints;
                        c.last_synced_at = Utc::now();
                    }
                    let snapshot = route_cache.read().unwrap().clone();
                    if let Err(e) = route_store.save(&snapshot).await {
                        warn!("Failed to save to AgentStore after route sync: {}", e);
                    }
                }
            });

            // Keep this thread alive forever
            tokio::signal::ctrl_c().await.ok();
        });
    });

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
