use axum::{
    Router, middleware,
    routing::{delete, get, post, put},
};
use chrono::Utc;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{info, warn};

use crate::AppState;
use crate::auth::{auth_middleware, rbac_middleware};
use crate::handlers::{
    cluster, drain, endpoints, exec, heartbeat, images, processes, register, resources, watch,
};
use crate::request_id::request_id_middleware;

use pkg_controllers::cronjob::CronJobController;
use pkg_controllers::daemonset::DaemonSetController;
use pkg_controllers::deployment::DeploymentController;
use pkg_controllers::eviction::EvictionController;
use pkg_controllers::hpa::HPAController;
use pkg_controllers::job::JobController;
use pkg_controllers::node::NodeController;
use pkg_controllers::replicaset::ReplicaSetController;
use pkg_metrics::MetricsRegistry;
use pkg_pki::ca::ClusterCA;
use pkg_scheduler::Scheduler;
use pkg_state::client::StateStore;
use pkg_state::leader::LeaderElection;

/// Server configuration passed from the binary's CLI.
pub struct ServerConfig {
    pub addr: SocketAddr,
    pub data_dir: String,
    pub join_token: String,
    pub node_name: String,
    pub server_id: String,
}

pub async fn start_server(config: ServerConfig) -> anyhow::Result<()> {
    // Initialize core subsystems
    let store = StateStore::new(&config.data_dir).await?;
    let ca = ClusterCA::new()?;
    let scheduler = Arc::new(Scheduler::new());

    // Initialize metrics registry
    let metrics = Arc::new(MetricsRegistry::new());
    metrics.register_counter("k3rs_api_requests_total", "Total API requests served");
    metrics.register_counter(
        "k3rs_controller_reconcile_total",
        "Total controller reconciliation cycles",
    );
    metrics.register_gauge("k3rs_nodes_total", "Total registered nodes");
    metrics.register_gauge("k3rs_pods_total", "Total pods in the cluster");
    metrics.register_gauge(
        "k3rs_leader_status",
        "Whether this server is the leader (1=leader, 0=follower)",
    );

    let state = AppState {
        store: store.clone(),
        ca: Arc::new(ca),
        join_token: config.join_token,
        listen_addr: config.addr.to_string(),
        scheduler: Some(scheduler.clone()),
        metrics,
    };

    // Seed default namespaces
    seed_default_namespaces(&store).await?;

    // Seed master node
    seed_master_node(&store, &config.node_name).await?;

    // Start leader election
    let election = LeaderElection::new(store.clone(), config.server_id.clone());
    let (_election_handle, leader_rx) = election.start();

    // Start leader-gated controllers
    let ctrl_store = store.clone();
    let ctrl_scheduler = scheduler.clone();
    tokio::spawn(async move {
        let mut rx = leader_rx;
        loop {
            // Wait until we become leader
            while !*rx.borrow() {
                if rx.changed().await.is_err() {
                    return;
                }
            }

            info!("Starting controllers (leader mode)");
            let handles = vec![
                NodeController::new(ctrl_store.clone()).start(),
                DeploymentController::new(ctrl_store.clone()).start(),
                ReplicaSetController::new(ctrl_store.clone(), ctrl_scheduler.clone()).start(),
                DaemonSetController::new(ctrl_store.clone()).start(),
                JobController::new(ctrl_store.clone(), ctrl_scheduler.clone()).start(),
                CronJobController::new(ctrl_store.clone()).start(),
                HPAController::new(ctrl_store.clone()).start(),
                EvictionController::new(ctrl_store.clone()).start(),
            ];

            // Wait until we lose leadership
            while *rx.borrow() {
                if rx.changed().await.is_err() {
                    return;
                }
            }

            warn!("Lost leadership — stopping controllers");
            for h in handles {
                h.abort();
            }
        }
    });

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
        // Node-scoped pod listing (all namespaces)
        .route(
            "/api/v1/nodes/{name}/pods",
            get(resources::list_node_pods),
        )
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
            "/api/v1/namespaces/{ns}/pods/{pod_name}",
            get(resources::get_pod).delete(resources::delete_pod),
        )
        .route(
            "/api/v1/namespaces/{ns}/pods/{pod_name}/status",
            put(resources::update_pod_status),
        )
        // Phase 4: pod logs
        .route(
            "/api/v1/namespaces/{ns}/pods/{pod_name}/logs",
            get(resources::pod_logs),
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
        // Phase 4: deployment CRUD
        .route(
            "/api/v1/namespaces/{ns}/deployments/{deploy_name}",
            get(resources::get_deployment).put(resources::update_deployment),
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
        // Phase 3: endpoints
        .route(
            "/api/v1/namespaces/{ns}/endpoints",
            post(endpoints::create_endpoint).get(endpoints::list_endpoints),
        )
        // Phase 3: ingresses
        .route(
            "/api/v1/namespaces/{ns}/ingresses",
            post(endpoints::create_ingress).get(endpoints::list_ingresses),
        )
        // Phase 4: replicasets
        .route(
            "/api/v1/namespaces/{ns}/replicasets",
            post(resources::create_replicaset).get(resources::list_replicasets),
        )
        // Phase 4: daemonsets
        .route(
            "/api/v1/namespaces/{ns}/daemonsets",
            post(resources::create_daemonset).get(resources::list_daemonsets),
        )
        // Phase 4: jobs
        .route(
            "/api/v1/namespaces/{ns}/jobs",
            post(resources::create_job).get(resources::list_jobs),
        )
        // Phase 4: cronjobs
        .route(
            "/api/v1/namespaces/{ns}/cronjobs",
            post(resources::create_cronjob).get(resources::list_cronjobs),
        )
        // Phase 4: hpa
        .route(
            "/api/v1/namespaces/{ns}/hpa",
            post(resources::create_hpa).get(resources::list_hpas),
        )
        // Phase 5: node drain/cordon/uncordon
        .route("/api/v1/nodes/{name}/cordon", post(drain::cordon_node))
        .route("/api/v1/nodes/{name}/uncordon", post(drain::uncordon_node))
        .route("/api/v1/nodes/{name}/drain", post(drain::drain_node))
        // Phase 5: resource quotas
        .route(
            "/api/v1/namespaces/{ns}/resourcequotas",
            post(resources::create_resource_quota).get(resources::list_resource_quotas),
        )
        // Phase 5: network policies
        .route(
            "/api/v1/namespaces/{ns}/networkpolicies",
            post(resources::create_network_policy).get(resources::list_network_policies),
        )
        // Phase 6: persistent volume claims
        .route(
            "/api/v1/namespaces/{ns}/pvcs",
            post(resources::create_pvc).get(resources::list_pvcs),
        )
        // Cluster: process list
        .route("/api/v1/processes", get(processes::list_processes))
        // Phase 7: exec into pod
        .route(
            "/api/v1/namespaces/{ns}/pods/{pod_name}/exec",
            get(exec::exec_into_pod),
        )
        // Runtime management
        .route(
            "/api/v1/runtime",
            get(crate::handlers::runtime::get_runtime_info),
        )
        .route(
            "/api/v1/runtime/upgrade",
            put(crate::handlers::runtime::upgrade_runtime),
        )
        // Image management
        .route("/api/v1/images", get(images::list_images))
        .route("/api/v1/images/pull", post(images::pull_image))
        .route("/api/v1/images/{image_id}", delete(images::delete_image))
        .route(
            "/api/v1/nodes/{name}/images",
            put(images::report_node_images),
        )
        // Phase 2: generic delete
        .route(
            "/api/v1/{resource_type}/{ns}/{name}",
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
        // Phase 6: Prometheus metrics endpoint (unprotected)
        .route("/metrics", get(metrics_handler))
        .merge(api_routes)
        .fallback(|req: axum::http::Request<axum::body::Body>| async move {
            warn!("No route matched for {} {}", req.method(), req.uri().path());
            axum::http::StatusCode::NOT_FOUND
        })
        .layer(middleware::from_fn(request_id_middleware))
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

/// Seed the master node on startup.
async fn seed_master_node(store: &StateStore, name: &str) -> anyhow::Result<()> {
    let key = format!("/registry/nodes/{}", name);
    if store.get(&key).await?.is_none() {
        let node = pkg_types::node::Node {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            status: pkg_types::node::NodeStatus::Ready,
            registered_at: Utc::now(),
            last_heartbeat: Utc::now(),
            labels: std::collections::HashMap::from([
                (
                    "node-role.kubernetes.io/master".to_string(),
                    "true".to_string(),
                ),
                (
                    "node-role.kubernetes.io/control-plane".to_string(),
                    "true".to_string(),
                ),
            ]),
            taints: vec![pkg_types::node::Taint {
                key: "node-role.kubernetes.io/control-plane".to_string(),
                value: String::new(),
                effect: pkg_types::pod::TaintEffect::NoSchedule,
            }],
            capacity: pkg_types::pod::ResourceRequirements::default(),
            allocated: pkg_types::pod::ResourceRequirements::default(),
            unschedulable: false,
        };
        let data = serde_json::to_vec(&node)?;
        store.put(&key, &data).await?;
        info!("Seeded master node");
    }
    Ok(())
}

/// Handler for `GET /metrics` — renders Prometheus text exposition format.
async fn metrics_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> impl axum::response::IntoResponse {
    // Update gauge values from live state
    let node_count = state
        .store
        .list_prefix("/registry/nodes/")
        .await
        .map(|e| e.len() as i64)
        .unwrap_or(0);
    let pod_count = state
        .store
        .list_prefix("/registry/pods/")
        .await
        .map(|e| e.len() as i64)
        .unwrap_or(0);

    state.metrics.gauge_set("k3rs_nodes_total", node_count);
    state.metrics.gauge_set("k3rs_pods_total", pod_count);
    state.metrics.counter_inc("k3rs_api_requests_total");

    let body = state.metrics.render();
    (
        axum::http::StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
}
