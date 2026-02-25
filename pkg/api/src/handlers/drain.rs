use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use pkg_types::node::{Node, NodeStatus};
use pkg_types::pod::{Pod, PodStatus};
use tracing::{info, warn};

use crate::AppState;

/// POST /api/v1/nodes/:name/cordon — mark a node as unschedulable.
pub async fn cordon_node(
    State(state): State<AppState>,
    Path(node_name): Path<String>,
) -> impl IntoResponse {
    info!("Cordon request for node: {}", node_name);

    match find_and_update_node(&state, &node_name, |node| {
        node.unschedulable = true;
        // Add unschedulable taint
        if !node
            .taints
            .iter()
            .any(|t| t.key == "node.kubernetes.io/unschedulable")
        {
            node.taints.push(pkg_types::node::Taint {
                key: "node.kubernetes.io/unschedulable".to_string(),
                value: "true".to_string(),
                effect: pkg_types::pod::TaintEffect::NoSchedule,
            });
        }
    })
    .await
    {
        Ok(_) => (
            StatusCode::OK,
            Json(serde_json::json!({"status": "cordoned"})),
        )
            .into_response(),
        Err(e) => {
            warn!("Cordon failed: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

/// POST /api/v1/nodes/:name/uncordon — mark a node as schedulable again.
pub async fn uncordon_node(
    State(state): State<AppState>,
    Path(node_name): Path<String>,
) -> impl IntoResponse {
    info!("Uncordon request for node: {}", node_name);

    match find_and_update_node(&state, &node_name, |node| {
        node.unschedulable = false;
        node.status = NodeStatus::Ready;
        node.taints
            .retain(|t| t.key != "node.kubernetes.io/unschedulable");
    })
    .await
    {
        Ok(_) => (
            StatusCode::OK,
            Json(serde_json::json!({"status": "uncordoned"})),
        )
            .into_response(),
        Err(e) => {
            warn!("Uncordon failed: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

/// POST /api/v1/nodes/:name/drain — cordon + evict all pods from the node.
pub async fn drain_node(
    State(state): State<AppState>,
    Path(node_name): Path<String>,
) -> impl IntoResponse {
    info!("Drain request for node: {}", node_name);

    // Step 1: Cordon the node
    if let Err(e) = find_and_update_node(&state, &node_name, |node| {
        node.unschedulable = true;
        node.status = NodeStatus::NotReady;
        if !node
            .taints
            .iter()
            .any(|t| t.key == "node.kubernetes.io/unschedulable")
        {
            node.taints.push(pkg_types::node::Taint {
                key: "node.kubernetes.io/unschedulable".to_string(),
                value: "true".to_string(),
                effect: pkg_types::pod::TaintEffect::NoSchedule,
            });
        }
    })
    .await
    {
        warn!("Drain failed during cordon: {}", e);
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    // Step 2: Evict all pods on this node
    let mut evicted = 0u32;
    // Scan all namespaces for pods on this node
    let entries = match state.store.list_prefix("/registry/pods/").await {
        Ok(e) => e,
        Err(e) => {
            warn!("Drain: failed to list pods: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to list pods").into_response();
        }
    };

    for (key, value) in entries {
        if let Ok(mut pod) = serde_json::from_slice::<Pod>(&value) {
            if pod.node_id.as_deref() == Some(&node_name) {
                info!("Evicting pod {} from node {}", pod.name, node_name);
                pod.node_id = None;
                pod.status = PodStatus::Pending;
                if let Ok(data) = serde_json::to_vec(&pod) {
                    let _ = state.store.put(&key, &data).await;
                    evicted += 1;
                }
            }
        }
    }

    info!(
        "Drain complete for node {}: {} pods evicted",
        node_name, evicted
    );
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "drained",
            "evicted_pods": evicted
        })),
    )
        .into_response()
}

/// Helper: find a node by name, apply a mutation, and persist it.
async fn find_and_update_node<F>(state: &AppState, node_name: &str, mutate: F) -> anyhow::Result<()>
where
    F: FnOnce(&mut Node),
{
    let entries = state.store.list_prefix("/registry/nodes/").await?;

    for (key, value) in entries {
        if let Ok(mut node) = serde_json::from_slice::<Node>(&value) {
            if node.name == node_name {
                mutate(&mut node);
                let data = serde_json::to_vec(&node)?;
                state.store.put(&key, &data).await?;
                return Ok(());
            }
        }
    }

    Err(anyhow::anyhow!("Node not found: {}", node_name))
}
