use std::net::Ipv4Addr;

use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::{TimeDelta, Utc};
use tracing::{info, warn};

use crate::AppState;
use pkg_types::vpc::{PeeringStatus, Vpc, VpcPeering, VpcStatus};

/// Parse an IPv4 CIDR string like "10.0.0.0/16" into (network_u32, prefix_len).
fn parse_cidr(cidr: &str) -> Option<(u32, u32)> {
    let (ip_str, prefix_str) = cidr.split_once('/')?;
    let ip: Ipv4Addr = ip_str.parse().ok()?;
    let prefix: u32 = prefix_str.parse().ok()?;
    if prefix > 32 {
        return None;
    }
    let mask = if prefix == 0 {
        0
    } else {
        !0u32 << (32 - prefix)
    };
    Some((u32::from(ip) & mask, prefix))
}

/// Check if two CIDRs overlap.
fn cidrs_overlap(a: &str, b: &str) -> bool {
    let Some((net_a, pfx_a)) = parse_cidr(a) else {
        return false;
    };
    let Some((net_b, pfx_b)) = parse_cidr(b) else {
        return false;
    };
    let common = pfx_a.min(pfx_b);
    let mask = if common == 0 {
        0
    } else {
        !0u32 << (32 - common)
    };
    (net_a & mask) == (net_b & mask)
}

// ============================================================
// VPCs
// ============================================================

pub async fn create_vpc(
    State(state): State<AppState>,
    Json(mut vpc): Json<Vpc>,
) -> impl IntoResponse {
    // Validate CIDR format
    if parse_cidr(&vpc.ipv4_cidr).is_none() {
        return (StatusCode::BAD_REQUEST, "Invalid CIDR format").into_response();
    }

    // Auto-allocate VpcID: scan existing VPCs for max ID, next = max + 1
    let entries = state
        .store
        .list_prefix("/registry/vpcs/")
        .await
        .unwrap_or_default();
    let existing_vpcs: Vec<Vpc> = entries
        .iter()
        .filter_map(|(_, v)| serde_json::from_slice::<Vpc>(v).ok())
        .collect();

    // Check CIDR overlap with existing VPCs
    for existing in &existing_vpcs {
        if cidrs_overlap(&vpc.ipv4_cidr, &existing.ipv4_cidr) {
            return (
                StatusCode::CONFLICT,
                format!(
                    "CIDR {} overlaps with VPC '{}' ({})",
                    vpc.ipv4_cidr, existing.name, existing.ipv4_cidr
                ),
            )
                .into_response();
        }
    }

    // Collect VpcIDs still in cooldown (Deleted < 300s ago)
    let now = Utc::now();
    let cooldown = TimeDelta::seconds(pkg_constants::timings::VPC_DELETION_COOLDOWN_SECS);
    let reserved_ids: std::collections::HashSet<u16> = existing_vpcs
        .iter()
        .filter(|v| {
            v.status == VpcStatus::Deleted
                && v.deleted_at
                    .is_some_and(|d| now.signed_duration_since(d) < cooldown)
        })
        .map(|v| v.vpc_id)
        .collect();

    let max_id = existing_vpcs.iter().map(|v| v.vpc_id).max().unwrap_or(0);

    // Pick next ID, skipping any in cooldown
    let mut next_id = max_id + 1;
    while reserved_ids.contains(&next_id) {
        next_id += 1;
    }
    vpc.vpc_id = next_id;
    vpc.status = VpcStatus::Active;
    vpc.created_at = Utc::now();
    vpc.deleted_at = None;

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
    if name == "default" {
        return (StatusCode::BAD_REQUEST, "Cannot delete the default VPC").into_response();
    }

    let key = format!("/registry/vpcs/{}", name);
    match state.store.get(&key).await {
        Ok(Some(data)) => {
            let mut vpc: Vpc = match serde_json::from_slice(&data) {
                Ok(v) => v,
                Err(_) => {
                    return (StatusCode::INTERNAL_SERVER_ERROR, "Deserialization failed")
                        .into_response();
                }
            };

            if vpc.status == VpcStatus::Terminating || vpc.status == VpcStatus::Deleted {
                return (StatusCode::OK, Json(vpc)).into_response();
            }

            // Transition to Terminating — no new pods, existing continue
            vpc.status = VpcStatus::Terminating;
            match serde_json::to_vec(&vpc) {
                Ok(updated) => {
                    if let Err(e) = state.store.put(&key, &updated).await {
                        warn!("Failed to update VPC status: {}", e);
                        return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to update VPC")
                            .into_response();
                    }
                    info!("VPC '{}' marked as Terminating (draining)", name);
                    (StatusCode::OK, Json(vpc)).into_response()
                }
                Err(_) => {
                    (StatusCode::INTERNAL_SERVER_ERROR, "Serialization failed").into_response()
                }
            }
        }
        Ok(None) => (StatusCode::NOT_FOUND, "VPC not found").into_response(),
        Err(e) => {
            warn!("Failed to get VPC for deletion: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "Store error").into_response()
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
