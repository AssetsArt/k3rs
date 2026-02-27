use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::AppState;

/// Image metadata reported by agents.
/// Mirrors `pkg_container::image::ImageInfo` but defined locally
/// so pkg-api does not depend on pkg-container.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageInfo {
    pub id: String,
    pub node_name: String,
    pub size: u64,
    pub size_human: String,
    pub layers: usize,
    pub architecture: String,
    pub os: String,
    pub created: String,
}

/// List all cached OCI images across all nodes.
/// Aggregates from state store where agents report their images.
pub async fn list_images(State(state): State<AppState>) -> impl IntoResponse {
    let mut all_images: Vec<ImageInfo> = Vec::new();

    // Collect images reported by agents (stored per-node in state)
    if let Ok(entries) = state.store.list_prefix("/registry/images/").await {
        for (_key, value) in entries {
            if let Ok(node_images) = serde_json::from_slice::<Vec<ImageInfo>>(&value) {
                all_images.extend(node_images);
            }
        }
    }

    // Sort: largest first
    all_images.sort_by_key(|i| std::cmp::Reverse(i.size));
    Json(all_images).into_response()
}

/// Agents call this to report their cached images.
/// PUT /api/v1/nodes/:name/images
pub async fn report_node_images(
    State(state): State<AppState>,
    AxumPath(node_name): AxumPath<String>,
    Json(mut images): Json<Vec<ImageInfo>>,
) -> impl IntoResponse {
    info!(
        "Node {} reporting {} cached images",
        node_name,
        images.len()
    );

    // Tag each image with the node name
    for img in &mut images {
        img.node_name = node_name.clone();
    }

    // Store in state
    let key = format!("/registry/images/{}", node_name);
    match serde_json::to_vec(&images) {
        Ok(data) => {
            if let Err(e) = state.store.put(&key, &data).await {
                return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
            }
            StatusCode::OK.into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Pull an image from a registry.
/// Image pulling is handled by the Agent where the pod will run.
#[derive(Debug, Deserialize)]
pub struct PullImageRequest {
    pub image: String,
}

pub async fn pull_image(Json(req): Json<PullImageRequest>) -> impl IntoResponse {
    info!(
        "Pull image request: {} — not available on control plane",
        req.image
    );
    (
        StatusCode::NOT_IMPLEMENTED,
        "Image pull is handled by the Agent node. \
         The server (control plane) does not pull or cache images.",
    )
}

/// Delete a cached image by ID.
/// Image management is handled by the Agent.
pub async fn delete_image(AxumPath(image_id): AxumPath<String>) -> impl IntoResponse {
    info!(
        "Delete image request: {} — not available on control plane",
        image_id
    );
    (
        StatusCode::NOT_IMPLEMENTED,
        "Image delete is handled by the Agent node. \
         The server (control plane) does not manage local images.",
    )
}
