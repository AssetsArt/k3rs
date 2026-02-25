use clap::Parser;
use pkg_proxy::tunnel::TunnelProxy;
use pkg_types::node::NodeRegistrationRequest;
use std::collections::HashMap;
use tracing::{error, info, warn};

#[derive(Parser, Debug)]
#[command(name = "k3rs-agent", about = "k3rs node agent (data plane)")]
struct Cli {
    /// Server API endpoint
    #[arg(long, default_value = "http://127.0.0.1:6443")]
    server: String,

    /// Join token for registration
    #[arg(long, default_value = "demo-token-123")]
    token: String,

    /// Node name
    #[arg(long, default_value = "node-1")]
    node_name: String,

    /// Local port for the tunnel proxy
    #[arg(long, default_value = "6444")]
    proxy_port: u16,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    info!("Starting k3rs-agent for node: {}", cli.node_name);

    // 1. Register with the Server
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true) // For development
        .build()?;

    let req = NodeRegistrationRequest {
        token: cli.token.clone(),
        node_name: cli.node_name.clone(),
        labels: HashMap::new(),
    };

    let url = format!("{}/register", cli.server.trim_end_matches('/'));
    info!("Registering with server at {}", url);

    match client.post(&url).json(&req).send().await {
        Ok(resp) => {
            if resp.status().is_success() {
                let reg_resp: pkg_types::node::NodeRegistrationResponse = resp.json().await?;
                info!("Successfully registered as node_id={}", reg_resp.node_id);
                info!(
                    "Certificate length: {} bytes, Key length: {} bytes",
                    reg_resp.certificate.len(),
                    reg_resp.private_key.len()
                );

                // Store certs to disk for future mTLS connections
                let cert_dir = format!("/tmp/k3rs-agent-{}", cli.node_name);
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
    // Parse the server host:port for the proxy upstream
    let server_host = cli
        .server
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    let proxy = TunnelProxy::new(server_host, cli.proxy_port);
    proxy.start().await?;

    // 3. Heartbeat loop — periodically report alive status
    info!("Starting heartbeat loop");
    let server_base = cli.server.clone();
    let node_name = cli.node_name.clone();
    let heartbeat_client = client.clone();

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(10));
        loop {
            interval.tick().await;
            let url = format!(
                "{}/api/v1/nodes/{}/heartbeat",
                server_base.trim_end_matches('/'),
                node_name
            );
            match heartbeat_client.put(&url).send().await {
                Ok(_) => {
                    info!("Heartbeat sent for {}", node_name);
                }
                Err(e) => {
                    warn!("Heartbeat failed: {}", e);
                }
            }
        }
    });

    // Block until Ctrl-C
    info!("Agent is running. Press Ctrl-C to stop.");
    tokio::signal::ctrl_c().await?;
    info!("Shutting down agent");

    Ok(())
}
