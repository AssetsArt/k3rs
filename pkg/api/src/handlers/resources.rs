use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::Utc;
use serde::Deserialize;
use tracing::{info, warn};
use uuid::Uuid;

use crate::AppState;

/// Query parameters for listing resources.
#[derive(Debug, Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub namespace: Option<String>,
}

// ============================================================
// Namespaces
// ============================================================

pub async fn create_namespace(
    State(state): State<AppState>,
    Json(mut ns): Json<pkg_types::namespace::Namespace>,
) -> impl IntoResponse {
    ns.created_at = Utc::now();
    let key = format!("/registry/namespaces/{}", ns.name);
    match serde_json::to_vec(&ns) {
        Ok(data) => {
            if let Err(e) = state.store.put(&key, &data).await {
                warn!("Failed to create namespace: {}", e);
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to create namespace",
                )
                    .into_response();
            }
            info!("Created namespace: {}", ns.name);
            (StatusCode::CREATED, Json(ns)).into_response()
        }
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Serialization failed").into_response(),
    }
}

pub async fn list_namespaces(State(state): State<AppState>) -> impl IntoResponse {
    let entries = state
        .store
        .list_prefix("/registry/namespaces/")
        .await
        .unwrap_or_default();
    let namespaces: Vec<pkg_types::namespace::Namespace> = entries
        .into_iter()
        .filter_map(|(_, v)| serde_json::from_slice(&v).ok())
        .collect();
    (StatusCode::OK, Json(namespaces)).into_response()
}

// ============================================================
// Pods
// ============================================================

pub async fn create_pod(
    State(state): State<AppState>,
    AxumPath(ns): AxumPath<String>,
    Json(mut pod): Json<pkg_types::pod::Pod>,
) -> impl IntoResponse {
    pod.id = Uuid::new_v4().to_string();
    pod.namespace = ns.clone();
    pod.status = pkg_types::pod::PodStatus::Pending;
    pod.created_at = Utc::now();

    // Schedule the pod if scheduler is available
    if let Some(ref scheduler) = state.scheduler {
        let entries = state
            .store
            .list_prefix("/registry/nodes/")
            .await
            .unwrap_or_default();
        let nodes: Vec<pkg_types::node::Node> = entries
            .into_iter()
            .filter_map(|(_, v)| serde_json::from_slice(&v).ok())
            .collect();
        if let Some(node_id) = scheduler.schedule(&pod, &nodes) {
            pod.node_id = Some(node_id);
            pod.status = pkg_types::pod::PodStatus::Scheduled;
        }
    }

    let key = format!("/registry/pods/{}/{}", ns, pod.id);
    match serde_json::to_vec(&pod) {
        Ok(data) => {
            if let Err(e) = state.store.put(&key, &data).await {
                warn!("Failed to create pod: {}", e);
                return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to create pod").into_response();
            }
            info!("Created pod {}/{} (id={})", ns, pod.name, pod.id);
            (StatusCode::CREATED, Json(pod)).into_response()
        }
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Serialization failed").into_response(),
    }
}

pub async fn list_pods(
    State(state): State<AppState>,
    AxumPath(ns): AxumPath<String>,
) -> impl IntoResponse {
    let prefix = format!("/registry/pods/{}/", ns);
    let entries = state.store.list_prefix(&prefix).await.unwrap_or_default();
    let pods: Vec<pkg_types::pod::Pod> = entries
        .into_iter()
        .filter_map(|(_, v)| serde_json::from_slice(&v).ok())
        .collect();
    (StatusCode::OK, Json(pods)).into_response()
}

pub async fn get_pod(
    State(state): State<AppState>,
    AxumPath((ns, pod_id)): AxumPath<(String, String)>,
) -> impl IntoResponse {
    let key = format!("/registry/pods/{}/{}", ns, pod_id);
    match state.store.get(&key).await {
        Ok(Some(data)) => {
            if let Ok(pod) = serde_json::from_slice::<pkg_types::pod::Pod>(&data) {
                return (StatusCode::OK, Json(pod)).into_response();
            }
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub async fn delete_pod(
    State(state): State<AppState>,
    AxumPath((ns, pod_id)): AxumPath<(String, String)>,
) -> impl IntoResponse {
    let key = format!("/registry/pods/{}/{}", ns, pod_id);
    match state.store.delete(&key).await {
        Ok(_) => {
            info!("Deleted pod {}/{}", ns, pod_id);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

// ============================================================
// Services
// ============================================================

pub async fn create_service(
    State(state): State<AppState>,
    AxumPath(ns): AxumPath<String>,
    Json(mut svc): Json<pkg_types::service::Service>,
) -> impl IntoResponse {
    svc.id = Uuid::new_v4().to_string();
    svc.namespace = ns.clone();
    svc.created_at = Utc::now();
    // Assign a cluster IP (simple increment for now)
    if svc.cluster_ip.is_none() {
        svc.cluster_ip = Some(format!(
            "10.43.0.{}",
            (uuid::Uuid::new_v4().as_bytes()[0] % 250) + 2
        ));
    }

    let key = format!("/registry/services/{}/{}", ns, svc.id);
    match serde_json::to_vec(&svc) {
        Ok(data) => {
            if let Err(e) = state.store.put(&key, &data).await {
                warn!("Failed to create service: {}", e);
                return (StatusCode::INTERNAL_SERVER_ERROR, "Failed").into_response();
            }
            info!("Created service {}/{}", ns, svc.name);
            (StatusCode::CREATED, Json(svc)).into_response()
        }
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Serialization failed").into_response(),
    }
}

pub async fn list_services(
    State(state): State<AppState>,
    AxumPath(ns): AxumPath<String>,
) -> impl IntoResponse {
    let prefix = format!("/registry/services/{}/", ns);
    let entries = state.store.list_prefix(&prefix).await.unwrap_or_default();
    let svcs: Vec<pkg_types::service::Service> = entries
        .into_iter()
        .filter_map(|(_, v)| serde_json::from_slice(&v).ok())
        .collect();
    (StatusCode::OK, Json(svcs)).into_response()
}

// ============================================================
// Deployments
// ============================================================

pub async fn create_deployment(
    State(state): State<AppState>,
    AxumPath(ns): AxumPath<String>,
    Json(mut deploy): Json<pkg_types::deployment::Deployment>,
) -> impl IntoResponse {
    deploy.id = Uuid::new_v4().to_string();
    deploy.namespace = ns.clone();
    deploy.created_at = Utc::now();

    let key = format!("/registry/deployments/{}/{}", ns, deploy.id);
    match serde_json::to_vec(&deploy) {
        Ok(data) => {
            if let Err(e) = state.store.put(&key, &data).await {
                warn!("Failed to create deployment: {}", e);
                return (StatusCode::INTERNAL_SERVER_ERROR, "Failed").into_response();
            }
            info!("Created deployment {}/{}", ns, deploy.name);
            (StatusCode::CREATED, Json(deploy)).into_response()
        }
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Serialization failed").into_response(),
    }
}

pub async fn list_deployments(
    State(state): State<AppState>,
    AxumPath(ns): AxumPath<String>,
) -> impl IntoResponse {
    let prefix = format!("/registry/deployments/{}/", ns);
    let entries = state.store.list_prefix(&prefix).await.unwrap_or_default();
    let deploys: Vec<pkg_types::deployment::Deployment> = entries
        .into_iter()
        .filter_map(|(_, v)| serde_json::from_slice(&v).ok())
        .collect();
    (StatusCode::OK, Json(deploys)).into_response()
}

// ============================================================
// ConfigMaps
// ============================================================

pub async fn create_configmap(
    State(state): State<AppState>,
    AxumPath(ns): AxumPath<String>,
    Json(mut cm): Json<pkg_types::configmap::ConfigMap>,
) -> impl IntoResponse {
    cm.id = Uuid::new_v4().to_string();
    cm.namespace = ns.clone();
    cm.created_at = Utc::now();

    let key = format!("/registry/configmaps/{}/{}", ns, cm.id);
    match serde_json::to_vec(&cm) {
        Ok(data) => {
            if let Err(_e) = state.store.put(&key, &data).await {
                return (StatusCode::INTERNAL_SERVER_ERROR, "Failed").into_response();
            }
            info!("Created configmap {}/{}", ns, cm.name);
            (StatusCode::CREATED, Json(cm)).into_response()
        }
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Serialization failed").into_response(),
    }
}

pub async fn list_configmaps(
    State(state): State<AppState>,
    AxumPath(ns): AxumPath<String>,
) -> impl IntoResponse {
    let prefix = format!("/registry/configmaps/{}/", ns);
    let entries = state.store.list_prefix(&prefix).await.unwrap_or_default();
    let cms: Vec<pkg_types::configmap::ConfigMap> = entries
        .into_iter()
        .filter_map(|(_, v)| serde_json::from_slice(&v).ok())
        .collect();
    (StatusCode::OK, Json(cms)).into_response()
}

// ============================================================
// Secrets
// ============================================================

pub async fn create_secret(
    State(state): State<AppState>,
    AxumPath(ns): AxumPath<String>,
    Json(mut secret): Json<pkg_types::secret::Secret>,
) -> impl IntoResponse {
    secret.id = Uuid::new_v4().to_string();
    secret.namespace = ns.clone();
    secret.created_at = Utc::now();

    let key = format!("/registry/secrets/{}/{}", ns, secret.id);
    match serde_json::to_vec(&secret) {
        Ok(data) => {
            if let Err(_e) = state.store.put(&key, &data).await {
                return (StatusCode::INTERNAL_SERVER_ERROR, "Failed").into_response();
            }
            info!("Created secret {}/{}", ns, secret.name);
            (StatusCode::CREATED, Json(secret)).into_response()
        }
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Serialization failed").into_response(),
    }
}

pub async fn list_secrets(
    State(state): State<AppState>,
    AxumPath(ns): AxumPath<String>,
) -> impl IntoResponse {
    let prefix = format!("/registry/secrets/{}/", ns);
    let entries = state.store.list_prefix(&prefix).await.unwrap_or_default();
    let secrets: Vec<pkg_types::secret::Secret> = entries
        .into_iter()
        .filter_map(|(_, v)| serde_json::from_slice(&v).ok())
        .collect();
    (StatusCode::OK, Json(secrets)).into_response()
}

// ============================================================
// Generic delete for any namespaced resource
// ============================================================

pub async fn delete_resource(
    State(state): State<AppState>,
    AxumPath((resource_type, ns, id)): AxumPath<(String, String, String)>,
) -> impl IntoResponse {
    let key = format!("/registry/{}/{}/{}", resource_type, ns, id);
    match state.store.delete(&key).await {
        Ok(_) => {
            info!("Deleted {}/{}/{}", resource_type, ns, id);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
