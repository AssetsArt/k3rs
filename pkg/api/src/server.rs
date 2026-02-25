use axum::{
    Router, middleware,
    routing::{delete, get, post, put},
};
use chrono::Utc;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::info;

use crate::AppState;
use crate::auth::{auth_middleware, rbac_middleware};
use crate::handlers::{cluster, heartbeat, register, resources, watch};
use pkg_controllers::node::NodeController;
use pkg_pki::ca::ClusterCA;
use pkg_scheduler::Scheduler;
use pkg_state::client::StateStore;

/// Server configuration passed from the binary's CLI.
pub struct ServerConfig {
    pub addr: SocketAddr,
    pub data_dir: String,
    pub join_token: String,
}

pub async fn start_server(config: ServerConfig) -> anyhow::Result<()> {
    // Initialize core subsystems
    let store = StateStore::new(&config.data_dir).await?;
    let ca = ClusterCA::new()?;
    let scheduler = Scheduler::new();

    let state = AppState {
        store: store.clone(),
        ca: Arc::new(ca),
        join_token: config.join_token,
        listen_addr: config.addr.to_string(),
        scheduler: Some(Arc::new(scheduler)),
    };

    // Seed default namespaces
    seed_default_namespaces(&store).await?;

    // Start the NodeController background task
    let node_controller = NodeController::new(store.clone());
    node_controller.start();

    // Protected API routes
    let api_routes = Router::new()
        // Phase 1: nodes
        .route("/api/v1/nodes", get(cluster::list_nodes))
        // Phase 2: heartbeat
        .route(
            "/api/v1/nodes/{name}/heartbeat",
            put(heartbeat::node_heartbeat),
        )
        // Phase 2: watch stream
        .route("/api/v1/watch", get(watch::watch_events))
        // Phase 2: namespaces
        .route(
            "/api/v1/namespaces",
            post(resources::create_namespace).get(resources::list_namespaces),
        )
        // Phase 2: pods
        .route(
            "/api/v1/namespaces/{ns}/pods",
            post(resources::create_pod).get(resources::list_pods),
        )
        .route(
            "/api/v1/namespaces/{ns}/pods/{pod_id}",
            get(resources::get_pod).delete(resources::delete_pod),
        )
        .route(
            "/api/v1/namespaces/{ns}/pods/{pod_id}/status",
            put(resources::update_pod_status),
        )
        // Phase 2: services
        .route(
            "/api/v1/namespaces/{ns}/services",
            post(resources::create_service).get(resources::list_services),
        )
        // Phase 2: deployments
        .route(
            "/api/v1/namespaces/{ns}/deployments",
            post(resources::create_deployment).get(resources::list_deployments),
        )
        // Phase 2: configmaps
        .route(
            "/api/v1/namespaces/{ns}/configmaps",
            post(resources::create_configmap).get(resources::list_configmaps),
        )
        // Phase 2: secrets
        .route(
            "/api/v1/namespaces/{ns}/secrets",
            post(resources::create_secret).get(resources::list_secrets),
        )
        // Phase 2: generic delete
        .route(
            "/api/v1/{resource_type}/{ns}/{id}",
            delete(resources::delete_resource),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            rbac_middleware,
        ))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ));

    // Public routes + merged
    let app = Router::new()
        // Phase 1: registration + cluster info (unprotected)
        .route("/register", post(register::register_node))
        .route("/api/v1/cluster/info", get(cluster::cluster_info))
        .merge(api_routes)
        .with_state(state);

    info!("Starting API server on {}", config.addr);
    let listener = TcpListener::bind(config.addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

/// Seed default and system namespaces on startup.
async fn seed_default_namespaces(store: &StateStore) -> anyhow::Result<()> {
    let namespaces = ["default", "k3rs-system"];
    for name in &namespaces {
        let key = format!("/registry/namespaces/{}", name);
        if store.get(&key).await?.is_none() {
            let ns = pkg_types::namespace::Namespace {
                name: name.to_string(),
                labels: std::collections::HashMap::new(),
                created_at: Utc::now(),
            };
            let data = serde_json::to_vec(&ns)?;
            store.put(&key, &data).await?;
            info!("Seeded namespace: {}", name);
        }
    }
    Ok(())
}
