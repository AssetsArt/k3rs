use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::Utc;
use tracing::{info, warn};

use crate::AppState;
use pkg_types::vpc::{PeeringStatus, Vpc, VpcPeering, VpcStatus};

// ============================================================
// VPCs
// ============================================================

pub async fn create_vpc(
    State(state): State<AppState>,
    Json(mut vpc): Json<Vpc>,
) -> impl IntoResponse {
    // Validate CIDR format (basic check: contains '/')
    if !vpc.ipv4_cidr.contains('/') {
        return (StatusCode::BAD_REQUEST, "Invalid CIDR format").into_response();
    }

    // Auto-allocate VpcID: scan existing VPCs for max ID, next = max + 1
    let entries = state
        .store
        .list_prefix("/registry/vpcs/")
        .await
        .unwrap_or_default();
    let max_id = entries
        .iter()
        .filter_map(|(_, v)| serde_json::from_slice::<Vpc>(v).ok())
        .map(|v| v.vpc_id)
        .max()
        .unwrap_or(0);
    vpc.vpc_id = max_id + 1;
    vpc.status = VpcStatus::Active;
    vpc.created_at = Utc::now();

    let key = format!("/registry/vpcs/{}", vpc.name);
    match serde_json::to_vec(&vpc) {
        Ok(data) => {
            if let Err(e) = state.store.put(&key, &data).await {
                warn!("Failed to create VPC: {}", e);
                return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to create VPC").into_response();
            }
            info!("Created VPC: {} (id={})", vpc.name, vpc.vpc_id);
            (StatusCode::CREATED, Json(vpc)).into_response()
        }
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Serialization failed").into_response(),
    }
}

pub async fn list_vpcs(State(state): State<AppState>) -> impl IntoResponse {
    let entries = state
        .store
        .list_prefix("/registry/vpcs/")
        .await
        .unwrap_or_default();
    let vpcs: Vec<Vpc> = entries
        .into_iter()
        .filter_map(|(_, v)| serde_json::from_slice(&v).ok())
        .collect();
    (StatusCode::OK, Json(vpcs)).into_response()
}

pub async fn get_vpc(
    State(state): State<AppState>,
    AxumPath(name): AxumPath<String>,
) -> impl IntoResponse {
    let key = format!("/registry/vpcs/{}", name);
    match state.store.get(&key).await {
        Ok(Some(data)) => match serde_json::from_slice::<Vpc>(&data) {
            Ok(vpc) => (StatusCode::OK, Json(vpc)).into_response(),
            Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Deserialization failed").into_response(),
        },
        Ok(None) => (StatusCode::NOT_FOUND, "VPC not found").into_response(),
        Err(e) => {
            warn!("Failed to get VPC: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "Store error").into_response()
        }
    }
}

pub async fn delete_vpc(
    State(state): State<AppState>,
    AxumPath(name): AxumPath<String>,
) -> impl IntoResponse {
    let key = format!("/registry/vpcs/{}", name);
    match state.store.delete(&key).await {
        Ok(_) => {
            info!("Deleted VPC: {}", name);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            warn!("Failed to delete VPC: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "Failed to delete VPC").into_response()
        }
    }
}

// ============================================================
// VPC Peerings
// ============================================================

pub async fn create_vpc_peering(
    State(state): State<AppState>,
    Json(mut peering): Json<VpcPeering>,
) -> impl IntoResponse {
    // Validate both VPCs exist
    let vpc_a_key = format!("/registry/vpcs/{}", peering.vpc_a);
    let vpc_b_key = format!("/registry/vpcs/{}", peering.vpc_b);

    match (
        state.store.get(&vpc_a_key).await,
        state.store.get(&vpc_b_key).await,
    ) {
        (Ok(Some(_)), Ok(Some(_))) => {}
        (Ok(None), _) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("VPC '{}' not found", peering.vpc_a),
            )
                .into_response();
        }
        (_, Ok(None)) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("VPC '{}' not found", peering.vpc_b),
            )
                .into_response();
        }
        (Err(e), _) | (_, Err(e)) => {
            warn!("Failed to validate VPCs for peering: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, "Store error").into_response();
        }
    }

    peering.status = PeeringStatus::Active;
    peering.created_at = Utc::now();

    let key = format!("/registry/vpc-peerings/{}", peering.name);
    match serde_json::to_vec(&peering) {
        Ok(data) => {
            if let Err(e) = state.store.put(&key, &data).await {
                warn!("Failed to create VPC peering: {}", e);
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to create VPC peering",
                )
                    .into_response();
            }
            info!(
                "Created VPC peering: {} ({} <-> {})",
                peering.name, peering.vpc_a, peering.vpc_b
            );
            (StatusCode::CREATED, Json(peering)).into_response()
        }
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Serialization failed").into_response(),
    }
}

pub async fn list_vpc_peerings(State(state): State<AppState>) -> impl IntoResponse {
    let entries = state
        .store
        .list_prefix("/registry/vpc-peerings/")
        .await
        .unwrap_or_default();
    let peerings: Vec<VpcPeering> = entries
        .into_iter()
        .filter_map(|(_, v)| serde_json::from_slice(&v).ok())
        .collect();
    (StatusCode::OK, Json(peerings)).into_response()
}

pub async fn delete_vpc_peering(
    State(state): State<AppState>,
    AxumPath(name): AxumPath<String>,
) -> impl IntoResponse {
    let key = format!("/registry/vpc-peerings/{}", name);
    match state.store.delete(&key).await {
        Ok(_) => {
            info!("Deleted VPC peering: {}", name);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            warn!("Failed to delete VPC peering: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to delete VPC peering",
            )
                .into_response()
        }
    }
}
