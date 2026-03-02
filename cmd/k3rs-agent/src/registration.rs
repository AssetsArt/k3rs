use pkg_types::node::{NodeRegistrationRequest, NodeRegistrationResponse};
use tracing::info;

/// Attempt registration with the server. Returns (node_id, agent_api_port, response) on success.
pub async fn try_register(
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
pub async fn try_connect(
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
