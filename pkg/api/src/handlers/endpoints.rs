use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::Utc;
use tracing::{info, warn};
use uuid::Uuid;

use crate::AppState;

// ============================================================
// Endpoints
// ============================================================

/// Create or update an Endpoint resource.
pub async fn create_endpoint(
    State(state): State<AppState>,
    AxumPath(ns): AxumPath<String>,
    Json(mut ep): Json<pkg_types::endpoint::Endpoint>,
) -> impl IntoResponse {
    ep.id = Uuid::new_v4().to_string();
    ep.namespace = ns.clone();
    ep.created_at = Utc::now();

    let key = format!("/registry/endpoints/{}/{}", ns, ep.service_id);
    match serde_json::to_vec(&ep) {
        Ok(data) => {
            if let Err(e) = state.store.put(&key, &data).await {
                warn!("Failed to create endpoint: {}", e);
                return (StatusCode::INTERNAL_SERVER_ERROR, "Failed").into_response();
            }
            info!(
                "Created endpoint for service {}/{} ({} addresses)",
                ns,
                ep.service_name,
                ep.addresses.len()
            );
            (StatusCode::CREATED, Json(ep)).into_response()
        }
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Serialization failed").into_response(),
    }
}

/// List all Endpoints in a namespace.
pub async fn list_endpoints(
    State(state): State<AppState>,
    AxumPath(ns): AxumPath<String>,
) -> impl IntoResponse {
    let prefix = format!("/registry/endpoints/{}/", ns);
    let entries = state.store.list_prefix(&prefix).await.unwrap_or_default();
    let eps: Vec<pkg_types::endpoint::Endpoint> = entries
        .into_iter()
        .filter_map(|(_, v)| serde_json::from_slice(&v).ok())
        .collect();
    (StatusCode::OK, Json(eps)).into_response()
}

// ============================================================
// Ingresses
// ============================================================

/// Create an Ingress resource.
pub async fn create_ingress(
    State(state): State<AppState>,
    AxumPath(ns): AxumPath<String>,
    Json(mut ingress): Json<pkg_types::ingress::Ingress>,
) -> impl IntoResponse {
    ingress.id = Uuid::new_v4().to_string();
    ingress.namespace = ns.clone();
    ingress.created_at = Utc::now();

    let key = format!("/registry/ingresses/{}/{}", ns, ingress.id);
    match serde_json::to_vec(&ingress) {
        Ok(data) => {
            if let Err(e) = state.store.put(&key, &data).await {
                warn!("Failed to create ingress: {}", e);
                return (StatusCode::INTERNAL_SERVER_ERROR, "Failed").into_response();
            }
            info!("Created ingress {}/{}", ns, ingress.name);
            (StatusCode::CREATED, Json(ingress)).into_response()
        }
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Serialization failed").into_response(),
    }
}

/// List all Ingresses in a namespace.
pub async fn list_ingresses(
    State(state): State<AppState>,
    AxumPath(ns): AxumPath<String>,
) -> impl IntoResponse {
    let prefix = format!("/registry/ingresses/{}/", ns);
    let entries = state.store.list_prefix(&prefix).await.unwrap_or_default();
    let ingresses: Vec<pkg_types::ingress::Ingress> = entries
        .into_iter()
        .filter_map(|(_, v)| serde_json::from_slice(&v).ok())
        .collect();
    (StatusCode::OK, Json(ingresses)).into_response()
}
