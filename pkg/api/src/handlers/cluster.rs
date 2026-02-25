use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use pkg_types::node::{ClusterInfo, Node};
use tracing::info;

use crate::AppState;

/// GET /api/v1/cluster/info — return cluster metadata.
pub async fn cluster_info(State(state): State<AppState>) -> impl IntoResponse {
    info!("Serving cluster info request");

    let nodes = state
        .store
        .list_prefix("/registry/nodes/")
        .await
        .unwrap_or_default();

    let info = ClusterInfo {
        endpoint: format!("http://{}", state.listen_addr),
        version: "v0.1.0+k3rs".to_string(),
        state_store: "SlateDB (local)".to_string(),
        node_count: nodes.len(),
    };

    (StatusCode::OK, Json(info)).into_response()
}

/// GET /api/v1/nodes — list all registered nodes.
pub async fn list_nodes(State(state): State<AppState>) -> impl IntoResponse {
    info!("Serving node list request");

    let entries = match state.store.list_prefix("/registry/nodes/").await {
        Ok(e) => e,
        Err(e) => {
            tracing::error!("Failed to list nodes: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to list nodes").into_response();
        }
    };

    let nodes: Vec<Node> = entries
        .into_iter()
        .filter_map(|(_key, value)| serde_json::from_slice::<Node>(&value).ok())
        .collect();

    (StatusCode::OK, Json(nodes)).into_response()
}
