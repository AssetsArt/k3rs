use clap::Parser;
use pkg_container::ContainerRuntime;
use pkg_proxy::service_proxy::ServiceProxy;
use pkg_proxy::tunnel::TunnelProxy;
use pkg_types::config::{AgentConfigFile, load_config_file};
use pkg_types::node::NodeRegistrationRequest;
use std::collections::HashMap;
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
    let _dns_port = cli.dns_port.or(file_cfg.dns_port).unwrap_or(5353);

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

    // 2. Start heartbeat immediately after registration (before any heavy init)
    //    Uses a dedicated OS thread + runtime because Pingora's run_forever()
    //    starves the main tokio runtime's task queue.
    let server_base = server.clone();
    let heartbeat_node_name = node_name.clone();
    let heartbeat_token = token.clone();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("heartbeat runtime");
        rt.block_on(async move {
            let client = reqwest::Client::new();
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(10));
            loop {
                interval.tick().await;
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
                    Ok(resp) => {
                        info!(
                            "Heartbeat OK for {} (status={})",
                            heartbeat_node_name,
                            resp.status()
                        );
                    }
                    Err(e) => {
                        warn!("Heartbeat failed for {}: {}", heartbeat_node_name, e);
                    }
                }
            }
        });
    });
    info!("Heartbeat loop started (every 10s)");

    // 3. Start the Pingora tunnel proxy
    let server_host = server
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    let proxy = TunnelProxy::new(server_host, proxy_port);
    proxy.start().await?;

    // 3. Start the Pingora Service Proxy (Phase 3)
    let service_proxy = Arc::new(ServiceProxy::new(service_proxy_port));
    service_proxy.start().await?;

    // 4. Start the embedded DNS server (Phase 3)
    /*
    let dns_addr: SocketAddr = format!("0.0.0.0:{}", dns_port).parse()?;
    let dns_server = Arc::new(DnsServer::new(dns_addr));
    dns_server.start().await?;
    */

    // 5. Agent controller loops — run in a dedicated thread with its own tokio runtime
    //    because Pingora's run_forever() starves the main runtime's task queue.
    info!("Starting node controllers (pod-sync, image-report, route-sync)");

    let ctrl_server = server.clone();
    let ctrl_token = token.clone();
    let ctrl_node_id = my_node_id.clone();
    let ctrl_service_proxy = service_proxy.clone();
    // let ctrl_dns_server = dns_server.clone();

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

            // Init container runtime (may download youki/crun)
            let runtime: Option<Arc<ContainerRuntime>> =
                match ContainerRuntime::new(None::<&str>).await {
                    Ok(rt) => {
                        info!("Container runtime ready: {}", rt.backend_name());

                        // --- Agent Recovery ---
                        info!("Starting agent recovery procedure...");
                        let discovered = rt.discover_running_containers().await.unwrap_or_default();

                        // Using ctrl_server, ctrl_token, ctrl_node_id here
                        let url = format!(
                            "{}/api/v1/nodes/{}/pods",
                            ctrl_server.trim_end_matches('/'),
                            ctrl_node_id
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
                                vec![]
                            }
                        };

                        let mut desired_running_ids = std::collections::HashMap::new();
                        for pod in &desired_pods {
                            desired_running_ids.insert(pod.id.clone(), (pod.name.clone(), pod.namespace.clone()));
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
                                let _ = rt.cleanup_container(&cid).await;
                            }
                        }
                        info!("Agent recovery complete.");
                        // ----------------------

                        Some(Arc::new(rt))
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
            let img_node = ctrl_node_id.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
                loop {
                    interval.tick().await;
                    let Some(ref rt) = img_runtime else { continue };
                    match rt.list_images().await {
                        Ok(images) => {
                            let url = format!(
                                "{}/api/v1/nodes/{}/images",
                                img_server.trim_end_matches('/'),
                                img_node
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

            // Pod sync loop
            let sync_client = client.clone();
            let sync_server = ctrl_server.clone();
            let sync_token = ctrl_token.clone();
            let sync_node_id = ctrl_node_id.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(5));
                loop {
                    interval.tick().await;

                    let url = format!(
                        "{}/api/v1/nodes/{}/pods",
                        sync_server.trim_end_matches('/'),
                        sync_node_id
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
                            // --- Health monitoring: check Running pods ---
                            if let Some(ref runtime) = runtime {
                                for pod in pods.iter().filter(|p| p.status == pkg_types::pod::PodStatus::Running) {
                                    match runtime.container_state(&pod.id).await {
                                        Ok(state) if state.status == "stopped" || state.status == "exited" => {
                                            warn!("[pod:{}] Container stopped unexpectedly", pod.name);

                                            // Log container output to help debugging
                                            if let Ok(logs) = runtime.container_logs(&pod.id, 20).await {
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
                                                .header("Authorization", format!("Bearer {}", sync_token))
                                                .json(&pkg_types::pod::PodStatus::Failed)
                                                .send()
                                                .await;
                                        }
                                        Err(_) => {
                                            // Container not found by runtime - likely crashed
                                            warn!("[pod:{}] Container not found in runtime", pod.name);
                                            let status_url = format!(
                                                "{}/api/v1/namespaces/{}/pods/{}/status",
                                                sync_server.trim_end_matches('/'),
                                                pod.namespace,
                                                pod.name
                                            );
                                            let _ = sync_client
                                                .put(&status_url)
                                                .header("Authorization", format!("Bearer {}", sync_token))
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

                                    let status_url = format!(
                                        "{}/api/v1/namespaces/{}/pods/{}/status",
                                        sync_server.trim_end_matches('/'),
                                        pod.namespace,
                                        pod.name
                                    );

                                    // Check runtime availability
                                    let Some(ref runtime) = runtime else {
                                        error!(
                                            "[pod:{}] No container runtime available!",
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
                                    let env =
                                        container_spec.map(|c| c.env.clone()).unwrap_or_default();

                                    // 1. Pull Image
                                    info!("[pod:{}] Pulling image: {}", pod.name, image);
                                    if let Err(e) = runtime.pull_image(image).await {
                                        error!("[pod:{}] Image pull failed: {}", pod.name, e);
                                        match sync_client
                                            .put(&status_url)
                                            .header(
                                                "Authorization",
                                                format!("Bearer {}", sync_token),
                                            )
                                            .json(&pkg_types::pod::PodStatus::Failed)
                                            .send()
                                            .await
                                        {
                                            Ok(resp) => {
                                                info!("[pod:{}] Status update to Failed (pull error): {}", pod.name, resp.status());
                                            }
                                            Err(e) => {
                                                error!("[pod:{}] Failed to update status: {}", pod.name, e);
                                            }
                                        }
                                        continue;
                                    }

                                    // 2. Create Container
                                    info!("[pod:{}] Creating container: {}", pod.name, pod.id);
                                    if let Err(e) = runtime
                                        .create_container(&pod.id, image, &command, &env)
                                        .await
                                    {
                                        error!(
                                            "[pod:{}] Container creation failed: {}",
                                            pod.name, e
                                        );
                                        match sync_client
                                            .put(&status_url)
                                            .header(
                                                "Authorization",
                                                format!("Bearer {}", sync_token),
                                            )
                                            .json(&pkg_types::pod::PodStatus::Failed)
                                            .send()
                                            .await
                                        {
                                            Ok(resp) => {
                                                info!("[pod:{}] Status update to Failed (create error): {}", pod.name, resp.status());
                                            }
                                            Err(e) => {
                                                error!("[pod:{}] Failed to update status: {}", pod.name, e);
                                            }
                                        }
                                        continue;
                                    }

                                    // 3. Start Container
                                    info!("[pod:{}] Starting container: {}", pod.name, pod.id);
                                    if let Err(e) = runtime.start_container(&pod.id).await {
                                        error!("[pod:{}] Container start failed: {}", pod.name, e);
                                        let _ = runtime.cleanup_container(&pod.id).await;
                                        match sync_client
                                            .put(&status_url)
                                            .header(
                                                "Authorization",
                                                format!("Bearer {}", sync_token),
                                            )
                                            .json(&pkg_types::pod::PodStatus::Failed)
                                            .send()
                                            .await
                                        {
                                            Ok(resp) => {
                                                info!("[pod:{}] Status update to Failed (start error): {}", pod.name, resp.status());
                                            }
                                            Err(e) => {
                                                error!("[pod:{}] Failed to update status: {}", pod.name, e);
                                            }
                                        }
                                        continue;
                                    }

                                    // 4. Success
                                    info!(
                                        "[pod:{}] Container running via {}",
                                        pod.name,
                                        runtime.backend_name()
                                    );
                                    match sync_client
                                        .put(&status_url)
                                        .header("Authorization", format!("Bearer {}", sync_token))
                                        .json(&pkg_types::pod::PodStatus::Running)
                                        .send()
                                        .await
                                    {
                                        Ok(resp) => {
                                            info!("[pod:{}] Status update to Running: {}", pod.name, resp.status());
                                        }
                                        Err(e) => {
                                            error!("[pod:{}] Failed to update status: {}", pod.name, e);
                                        }
                                    }
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
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(10));
                loop {
                    interval.tick().await;

                    let base = route_server.trim_end_matches('/');
                    let auth = format!("Bearer {}", route_token);

                    // Fetch all namespaces to sync routes across all of them
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

                    ctrl_service_proxy
                        .update_routes(&all_services, &all_endpoints)
                        .await;
                    // ctrl_dns_server.update_records(&all_services).await;
                }
            });

            // Keep this thread alive forever
            tokio::signal::ctrl_c().await.ok();
        });
    });

    // Block until Ctrl-C
    info!("Agent is running. Press Ctrl-C to stop.");
    tokio::signal::ctrl_c().await?;
    info!("Shutting down agent");

    Ok(())
}
