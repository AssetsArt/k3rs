use clap::Parser;
use pkg_container::ContainerRuntime;
use pkg_network::dns::DnsServer;
use pkg_proxy::service_proxy::ServiceProxy;
use pkg_proxy::tunnel::TunnelProxy;
use pkg_types::config::{AgentConfigFile, load_config_file};
use pkg_types::node::NodeRegistrationRequest;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::{error, info, warn};

#[derive(Parser, Debug)]
#[command(name = "k3rs-agent", about = "k3rs node agent (data plane)")]
struct Cli {
    /// Path to YAML config file
    #[arg(long, short, default_value = "/etc/k3rs/agent-config.yaml")]
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
        .unwrap_or_else(|| "http://127.0.0.1:6443".to_string());
    let token = cli
        .token
        .or(file_cfg.token)
        .unwrap_or_else(|| "demo-token-123".to_string());
    let node_name = cli
        .node_name
        .or(file_cfg.node_name)
        .unwrap_or_else(|| "node-1".to_string());
    let proxy_port = cli.proxy_port.or(file_cfg.proxy_port).unwrap_or(6444);
    let service_proxy_port = cli
        .service_proxy_port
        .or(file_cfg.service_proxy_port)
        .unwrap_or(10256);
    let dns_port = cli.dns_port.or(file_cfg.dns_port).unwrap_or(5353);

    info!("Starting k3rs-agent for node: {}", node_name);

    // 1. Register with the Server
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true) // For development
        .build()?;

    let req = NodeRegistrationRequest {
        token: token.clone(),
        node_name: node_name.clone(),
        labels: HashMap::new(),
    };

    let url = format!("{}/register", server.trim_end_matches('/'));
    info!("Registering with server at {}", url);

    let my_node_id: String;
    match client.post(&url).json(&req).send().await {
        Ok(resp) => {
            if resp.status().is_success() {
                let reg_resp: pkg_types::node::NodeRegistrationResponse = resp.json().await?;
                my_node_id = reg_resp.node_id.clone();
                info!("Successfully registered as node_id={}", my_node_id);
                info!(
                    "Certificate length: {} bytes, Key length: {} bytes",
                    reg_resp.certificate.len(),
                    reg_resp.private_key.len()
                );

                // Store certs to disk for future mTLS connections
                let cert_dir = format!("/tmp/k3rs-agent-{}", node_name);
                tokio::fs::create_dir_all(&cert_dir).await?;
                tokio::fs::write(format!("{}/node.crt", cert_dir), &reg_resp.certificate).await?;
                tokio::fs::write(format!("{}/node.key", cert_dir), &reg_resp.private_key).await?;
                tokio::fs::write(format!("{}/ca.crt", cert_dir), &reg_resp.server_ca).await?;
                info!("Certificates saved to {}", cert_dir);
            } else {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                error!("Registration failed: {} - {}", status, body);
                return Err(anyhow::anyhow!("Registration failed: {}", status));
            }
        }
        Err(e) => {
            error!(
                "Failed to connect to server: {}. Is k3rs-server running?",
                e
            );
            return Err(e.into());
        }
    }

    // 2. Start the Pingora tunnel proxy
    let server_host = server
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    let proxy = TunnelProxy::new(server_host, proxy_port);
    proxy.start().await?;

    // 3. Start the Pingora Service Proxy (Phase 3)
    let service_proxy = Arc::new(ServiceProxy::new(service_proxy_port));
    service_proxy.start().await?;

    // 4. Start the embedded DNS server (Phase 3)
    let dns_addr: SocketAddr = format!("0.0.0.0:{}", dns_port).parse()?;
    let dns_server = Arc::new(DnsServer::new(dns_addr));
    dns_server.start().await?;

    // 5. Heartbeat and Pod Sync loops
    info!("Starting node controllers (heartbeat, pod-sync, route-sync)");

    // Heartbeat loop
    let server_base = server.clone();
    let heartbeat_node_name = node_name.clone();
    let heartbeat_client = client.clone();
    let token_clone = token.clone();

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(10));
        loop {
            interval.tick().await;
            let url = format!(
                "{}/api/v1/nodes/{}/heartbeat",
                server_base.trim_end_matches('/'),
                heartbeat_node_name
            );
            match heartbeat_client
                .put(&url)
                .header("Authorization", format!("Bearer {}", token_clone))
                .send()
                .await
            {
                Ok(_) => {
                    info!("Heartbeat sent for {}", heartbeat_node_name);
                }
                Err(e) => {
                    warn!("Heartbeat failed: {}", e);
                }
            }
        }
    });

    // Image report loop — report cached images to server every 30s
    let img_server = server.clone();
    let img_node_name = node_name.clone();
    let img_client = client.clone();
    let img_token = token.clone();

    // Pod Sync loop
    let runtime = Arc::new(ContainerRuntime::new(None::<&str>).await?);

    let img_runtime = runtime.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
        loop {
            interval.tick().await;
            match img_runtime.list_images().await {
                Ok(images) => {
                    let url = format!(
                        "{}/api/v1/nodes/{}/images",
                        img_server.trim_end_matches('/'),
                        img_node_name
                    );
                    match img_client
                        .put(&url)
                        .header("Authorization", format!("Bearer {}", img_token))
                        .json(&images)
                        .send()
                        .await
                    {
                        Ok(_) => info!("Reported {} images for {}", images.len(), img_node_name),
                        Err(e) => warn!("Image report failed: {}", e),
                    }
                }
                Err(e) => warn!("Failed to list images: {}", e),
            }
        }
    });
    let sync_client = client.clone();
    let sync_server_base = server.clone();
    let sync_token = token.clone();

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(5));
        loop {
            interval.tick().await;

            // For now, poll default namespace
            let ns = "default";
            let url = format!(
                "{}/api/v1/namespaces/{}/pods",
                sync_server_base.trim_end_matches('/'),
                ns
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

            if let Ok(pods) = resp.json::<Vec<pkg_types::pod::Pod>>().await {
                for pod in pods {
                    // Only process pods assigned to this node and not yet running
                    if pod.node_name.as_deref() == Some(my_node_id.as_str())
                        && (pod.status == pkg_types::pod::PodStatus::Scheduled
                            || pod.status == pkg_types::pod::PodStatus::ContainerCreating)
                    {
                        info!(
                            "Found new scheduled pod: {} (image: {})",
                            pod.name,
                            pod.spec
                                .containers
                                .first()
                                .map(|c| c.image.as_str())
                                .unwrap_or("unknown")
                        );

                        let status_url = format!(
                            "{}/api/v1/namespaces/{}/pods/{}/status",
                            sync_server_base.trim_end_matches('/'),
                            ns,
                            pod.id
                        );

                        // Extract container spec from pod
                        let container_spec = pod.spec.containers.first();
                        let image = container_spec
                            .map(|c| c.image.as_str())
                            .unwrap_or("alpine:latest");
                        let command: Vec<String> = container_spec
                            .map(|c| {
                                let mut cmd = c.command.clone();
                                cmd.extend(c.args.clone());
                                cmd
                            })
                            .unwrap_or_default();
                        let env = container_spec.map(|c| c.env.clone()).unwrap_or_default();

                        // 1. Pull Image
                        info!("[pod:{}] Pulling image: {}", pod.name, image);
                        if let Err(e) = runtime.pull_image(image).await {
                            error!("[pod:{}] Image pull failed: {}", pod.name, e);
                            let _ = sync_client
                                .put(&status_url)
                                .header("Authorization", format!("Bearer {}", sync_token))
                                .json(&serde_json::json!({
                                    "status": "Failed",
                                    "status_message": format!("ImagePullError: {}", e)
                                }))
                                .send()
                                .await;
                            continue;
                        }

                        // 2. Create Container
                        info!("[pod:{}] Creating container: {}", pod.name, pod.id);
                        if let Err(e) = runtime
                            .create_container(&pod.id, image, &command, &env)
                            .await
                        {
                            error!("[pod:{}] Container creation failed: {}", pod.name, e);
                            let _ = sync_client
                                .put(&status_url)
                                .header("Authorization", format!("Bearer {}", sync_token))
                                .json(&serde_json::json!({
                                    "status": "Failed",
                                    "status_message": format!("ContainerCreateError: {}", e)
                                }))
                                .send()
                                .await;
                            continue;
                        }

                        // 3. Start Container
                        info!("[pod:{}] Starting container: {}", pod.name, pod.id);
                        if let Err(e) = runtime.start_container(&pod.id).await {
                            error!("[pod:{}] Container start failed: {}", pod.name, e);
                            // Clean up the created-but-failed container
                            let _ = runtime.cleanup_container(&pod.id).await;
                            let _ = sync_client
                                .put(&status_url)
                                .header("Authorization", format!("Bearer {}", sync_token))
                                .json(&serde_json::json!({
                                    "status": "Failed",
                                    "status_message": format!("ContainerStartError: {}", e)
                                }))
                                .send()
                                .await;
                            continue;
                        }

                        // 4. Success — Update Status to Running
                        info!(
                            "[pod:{}] Container running via {}",
                            pod.name,
                            runtime.backend_name()
                        );
                        let new_status = pkg_types::pod::PodStatus::Running;
                        let _ = sync_client
                            .put(&status_url)
                            .header("Authorization", format!("Bearer {}", sync_token))
                            .json(&new_status)
                            .send()
                            .await;
                    }
                }
            }
        }
    });

    // Route Sync loop (Phase 3) — synchronize Service Proxy and DNS with cluster state
    let route_sync_client = client.clone();
    let route_sync_server = server.clone();
    let route_sync_token = token.clone();
    let route_service_proxy = service_proxy.clone();
    let route_dns_server = dns_server.clone();

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(10));
        loop {
            interval.tick().await;

            let ns = "default";
            let base = route_sync_server.trim_end_matches('/');
            let auth_header = format!("Bearer {}", route_sync_token);

            // Fetch services
            let services_url = format!("{}/api/v1/namespaces/{}/services", base, ns);
            let services: Vec<pkg_types::service::Service> = match route_sync_client
                .get(&services_url)
                .header("Authorization", &auth_header)
                .send()
                .await
            {
                Ok(r) => r.json().await.unwrap_or_default(),
                Err(e) => {
                    warn!("Route sync: failed to fetch services: {}", e);
                    continue;
                }
            };

            // Fetch endpoints
            let endpoints_url = format!("{}/api/v1/namespaces/{}/endpoints", base, ns);
            let endpoints: Vec<pkg_types::endpoint::Endpoint> = match route_sync_client
                .get(&endpoints_url)
                .header("Authorization", &auth_header)
                .send()
                .await
            {
                Ok(r) => r.json().await.unwrap_or_default(),
                Err(e) => {
                    warn!("Route sync: failed to fetch endpoints: {}", e);
                    continue;
                }
            };

            // Update Service Proxy routing table
            route_service_proxy
                .update_routes(&services, &endpoints)
                .await;

            // Update DNS records
            route_dns_server.update_records(&services).await;
        }
    });

    // Block until Ctrl-C
    info!("Agent is running. Press Ctrl-C to stop.");
    tokio::signal::ctrl_c().await?;
    info!("Shutting down agent");

    Ok(())
}
