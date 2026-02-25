use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::IntoResponse,
};
use pkg_container::image::ImageInfo;
use serde::Deserialize;
use tracing::info;

use crate::AppState;

/// List all cached OCI images across all nodes.
/// Aggregates from state store where agents report their images.
pub async fn list_images(State(state): State<AppState>) -> impl IntoResponse {
    let mut all_images: Vec<ImageInfo> = Vec::new();

    // 1. Collect images reported by agents (stored per-node in state)
    if let Ok(entries) = state.store.list_prefix("/registry/images/").await {
        for (_key, value) in entries {
            if let Ok(node_images) = serde_json::from_slice::<Vec<ImageInfo>>(&value) {
                all_images.extend(node_images);
            }
        }
    }

    // 2. Also include local server images (if any)
    if let Ok(local_images) = state.container_runtime.list_images().await {
        let hostname = std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("COMPUTERNAME"))
            .unwrap_or_else(|_| "server".to_string());
        for mut img in local_images {
            if img.node_name.is_empty() {
                img.node_name = hostname.clone();
            }
            all_images.push(img);
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

/// Pull an image from a registry (on the server node).
#[derive(Debug, Deserialize)]
pub struct PullImageRequest {
    pub image: String,
}

pub async fn pull_image(
    State(state): State<AppState>,
    Json(req): Json<PullImageRequest>,
) -> impl IntoResponse {
    info!("Pull image request: {}", req.image);
    match state.container_runtime.pull_image(&req.image).await {
        Ok(()) => (
            StatusCode::OK,
            format!("Image {} pulled successfully", req.image),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to pull {}: {}", req.image, e),
        )
            .into_response(),
    }
}

/// Delete a cached image by ID (from server node).
pub async fn delete_image(
    State(state): State<AppState>,
    AxumPath(image_id): AxumPath<String>,
) -> impl IntoResponse {
    info!("Delete image request: {}", image_id);
    match state.container_runtime.delete_image(&image_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    }
}
