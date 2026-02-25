use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use chrono::Utc;
use pkg_types::node::{Node, NodeRegistrationRequest, NodeRegistrationResponse, NodeStatus};
use pkg_types::pod::ResourceRequirements;
use tracing::{info, warn};

use crate::AppState;

pub async fn register_node(
    State(state): State<AppState>,
    Json(payload): Json<NodeRegistrationRequest>,
) -> impl IntoResponse {
    info!(
        "Received registration request for node: {}",
        payload.node_name
    );

    // Verify join token
    if payload.token.is_empty() {
        warn!(
            "Node {} attempted to register without a token",
            payload.node_name
        );
        return (StatusCode::UNAUTHORIZED, "Missing join token").into_response();
    }

    if payload.token != state.join_token {
        warn!("Node {} provided an invalid join token", payload.node_name);
        return (StatusCode::FORBIDDEN, "Invalid join token").into_response();
    }

    // Issue a real certificate via the CA
    let (cert_pem, key_pem) = match state.ca.issue_node_cert(&payload.node_name) {
        Ok(pair) => pair,
        Err(e) => {
            tracing::error!("Failed to issue certificate: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Certificate generation failed",
            )
                .into_response();
        }
    };

    // Check if node already exists
    let key = format!("/registry/nodes/{}", payload.node_name);
    let existing_node = match state.store.get(&key).await {
        Ok(Some(data)) => serde_json::from_slice::<Node>(&data).ok(),
        _ => None,
    };

    let now = Utc::now();
    let node_id = payload.node_name.clone();

    let node = if let Some(mut existing) = existing_node {
        info!("Updating existing node: {}", node_id);
        existing.status = NodeStatus::Ready;
        existing.last_heartbeat = now;
        existing.labels.extend(payload.labels.clone());
        existing
    } else {
        Node {
            id: node_id.clone(),
            name: payload.node_name.clone(),
            status: NodeStatus::Ready,
            registered_at: now,
            last_heartbeat: now,
            labels: payload.labels.clone(),
            taints: vec![],
            capacity: ResourceRequirements::default(),
            allocated: ResourceRequirements::default(),
            unschedulable: false,
        }
    };

    match serde_json::to_vec(&node) {
        Ok(data) => {
            if let Err(e) = state.store.put(&key, &data).await {
                tracing::error!("Failed to persist node: {}", e);
                return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to persist node")
                    .into_response();
            }
        }
        Err(e) => {
            tracing::error!("Failed to serialize node: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, "Serialization failed").into_response();
        }
    }

    info!("Node {} registered with id {}", payload.node_name, node_id);

    let response = NodeRegistrationResponse {
        node_id,
        certificate: cert_pem,
        private_key: key_pem,
        server_ca: state.ca.ca_cert_pem().to_string(),
    };

    (StatusCode::OK, Json(response)).into_response()
}
