use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::Utc;
use pkg_types::node::{Node, NodeStatus};
use tracing::{info, warn};

use crate::AppState;

/// PUT /api/v1/nodes/:name/heartbeat — update node heartbeat timestamp.
pub async fn node_heartbeat(
    State(state): State<AppState>,
    Path(node_name): Path<String>,
) -> impl IntoResponse {
    // Find the node by name
    let entries = match state.store.list_prefix("/registry/nodes/").await {
        Ok(e) => e,
        Err(e) => {
            warn!("Failed to list nodes for heartbeat: {}", e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    for (key, value) in entries {
        if let Ok(mut node) = serde_json::from_slice::<Node>(&value) {
            if node.name == node_name {
                node.last_heartbeat = Utc::now();
                node.status = NodeStatus::Ready;
                match serde_json::to_vec(&node) {
                    Ok(data) => {
                        if let Err(e) = state.store.put(&key, &data).await {
                            warn!("Failed to update heartbeat: {}", e);
                            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                        }
                        return (StatusCode::OK, Json(serde_json::json!({"status": "ok"})))
                            .into_response();
                    }
                    Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                }
            }
        }
    }

    info!("Heartbeat for unknown node: {}", node_name);
    StatusCode::NOT_FOUND.into_response()
}
