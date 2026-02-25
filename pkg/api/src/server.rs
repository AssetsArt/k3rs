use axum::{
    Router,
    routing::{get, post},
};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::info;

use crate::AppState;
use crate::handlers::{cluster, register};
use pkg_pki::ca::ClusterCA;
use pkg_state::client::StateStore;

/// Server configuration passed from the binary's CLI.
pub struct ServerConfig {
    pub addr: SocketAddr,
    pub data_dir: String,
    pub join_token: String,
}

pub async fn start_server(config: ServerConfig) -> anyhow::Result<()> {
    // Initialize core subsystems
    let store = StateStore::new(&config.data_dir).await?;
    let ca = ClusterCA::new()?;

    let state = AppState {
        store,
        ca: Arc::new(ca),
        join_token: config.join_token,
        listen_addr: config.addr.to_string(),
    };

    let app = Router::new()
        .route("/register", post(register::register_node))
        .route("/api/v1/cluster/info", get(cluster::cluster_info))
        .route("/api/v1/nodes", get(cluster::list_nodes))
        .with_state(state);

    info!("Starting API server on {}", config.addr);
    let listener = TcpListener::bind(config.addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
