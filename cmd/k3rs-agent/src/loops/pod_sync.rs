use crate::cache::AgentStateCache;
use crate::connectivity::ConnectivityManager;
use crate::store::AgentStore;
use crate::vpc_client::VpcClient;
use chrono::Utc;
use pkg_container::ContainerRuntime;
use std::sync::Arc;
use tracing::{error, info, warn};

/// Start the pod sync loop (every 5s).
#[allow(clippy::too_many_arguments)]
pub fn start(
    runtime: Option<Arc<ContainerRuntime>>,
    client: reqwest::Client,
    server: String,
    token: String,
    node_name: String,
    cache: Arc<std::sync::RwLock<AgentStateCache>>,
    connectivity: Arc<ConnectivityManager>,
    store: AgentStore,
    vpc_client: Arc<VpcClient>,
) {
    let in_flight: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(5));
        loop {
            interval.tick().await;

            // Skip when not connected — do not create containers from stale cache
            if !connectivity.is_connected() {
                continue;
            }

            // Pod sync: skip if not registered yet
            let has_registration = cache.read().unwrap().node_id.is_some();
            if !has_registration {
                continue;
            };

            let url = format!(
                "{}/api/v1/pods?fieldSelector=spec.nodeName={}",
                server.trim_end_matches('/'),
                node_name
            );

            let resp = match client
                .get(&url)
                .header("Authorization", format!("Bearer {}", token))
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    warn!("Pod sync failed to fetch pods: {}", e);
                    continue;
                }
            };

            match resp.json::<Vec<pkg_types::pod::Pod>>().await {
                Ok(pods) => {
                    // Update in-memory cache with fetched pods (outside lock for save)
                    {
                        let mut c = cache.write().unwrap();
                        c.pods = pods.clone();
                        c.last_synced_at = Utc::now();
                    }
                    let snapshot = cache.read().unwrap().clone();
                    if let Err(e) = store.save(&snapshot).await {
                        warn!("Failed to save to AgentStore after pod sync: {}", e);
                    }

                    // --- Health monitoring: check Running pods ---
                    if let Some(ref runtime) = runtime {
                        check_running_pods(&pods, runtime, &client, &server, &token, &vpc_client)
                            .await;
                    }

                    // --- Schedule new pods ---
                    schedule_new_pods(
                        &pods,
                        &runtime,
                        &client,
                        &server,
                        &token,
                        &in_flight,
                        &vpc_client,
                    );
                }
                Err(e) => {
                    warn!("Failed to parse pods from JSON: {}", e);
                }
            }
        }
    });
}

/// Check health of Running pods and report failures.
async fn check_running_pods(
    pods: &[pkg_types::pod::Pod],
    runtime: &Arc<ContainerRuntime>,
    client: &reqwest::Client,
    server: &str,
    token: &str,
    vpc_client: &Arc<VpcClient>,
) {
    for pod in pods
        .iter()
        .filter(|p| p.status == pkg_types::pod::PodStatus::Running)
    {
        match runtime.container_state(&pod.id).await {
            Ok(state) if state.status == "stopped" || state.status == "exited" => {
                warn!("[pod:{}] Container stopped unexpectedly", pod.name);

                // Release VPC allocation (best-effort)
                let vpc_name = pod
                    .vpc_name
                    .as_deref()
                    .or(pod.spec.vpc.as_deref())
                    .unwrap_or("default");
                if let Err(e) = vpc_client.release(&pod.id, vpc_name).await {
                    warn!("[pod:{}] VPC release failed: {}", pod.name, e);
                }

                if let Ok(logs) = runtime.container_logs(&pod.id, 20).await {
                    for line in logs {
                        warn!("[pod:{}]   > {}", pod.name, line);
                    }
                }

                let status_url = format!(
                    "{}/api/v1/namespaces/{}/pods/{}/status",
                    server.trim_end_matches('/'),
                    pod.namespace,
                    pod.name
                );
                let _ = client
                    .put(&status_url)
                    .header("Authorization", format!("Bearer {}", token))
                    .json(&pkg_types::pod::PodStatus::Failed)
                    .send()
                    .await;
            }
            Err(_) => {
                warn!("[pod:{}] Container not found in runtime", pod.name);

                // Release VPC allocation (best-effort)
                let vpc_name = pod
                    .vpc_name
                    .as_deref()
                    .or(pod.spec.vpc.as_deref())
                    .unwrap_or("default");
                if let Err(e) = vpc_client.release(&pod.id, vpc_name).await {
                    warn!("[pod:{}] VPC release failed: {}", pod.name, e);
                }

                let status_url = format!(
                    "{}/api/v1/namespaces/{}/pods/{}/status",
                    server.trim_end_matches('/'),
                    pod.namespace,
                    pod.name
                );
                let _ = client
                    .put(&status_url)
                    .header("Authorization", format!("Bearer {}", token))
                    .json(&pkg_types::pod::PodStatus::Failed)
                    .send()
                    .await;
            }
            _ => {} // Still running, all good
        }
    }
}

/// Schedule pods that are in Scheduled or ContainerCreating status.
fn schedule_new_pods(
    pods: &[pkg_types::pod::Pod],
    runtime: &Option<Arc<ContainerRuntime>>,
    client: &reqwest::Client,
    server: &str,
    token: &str,
    in_flight: &std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    vpc_client: &Arc<VpcClient>,
) {
    for pod in pods {
        if pod.status == pkg_types::pod::PodStatus::Scheduled
            || pod.status == pkg_types::pod::PodStatus::ContainerCreating
        {
            info!(
                "Found scheduled pod: {} (image: {})",
                pod.name,
                pod.spec
                    .containers
                    .first()
                    .map(|c| c.image.as_str())
                    .unwrap_or("unknown")
            );

            let Some(rt_arc) = runtime else {
                error!("[pod:{}] No container runtime available!", pod.name);
                let status_url = format!(
                    "{}/api/v1/namespaces/{}/pods/{}/status",
                    server.trim_end_matches('/'),
                    pod.namespace,
                    pod.name
                );
                let pod_client = client.clone();
                let pod_token = token.to_string();
                tokio::spawn(async move {
                    let _ = pod_client
                        .put(&status_url)
                        .header("Authorization", format!("Bearer {}", pod_token))
                        .json(&pkg_types::pod::PodStatus::Failed)
                        .send()
                        .await;
                });
                continue;
            };

            let pod_runtime = rt_arc.clone();
            let pod_client = client.clone();
            let pod_server = server.to_string();
            let pod_token = token.to_string();
            let pod_in_flight = in_flight.clone();
            let pod_vpc = vpc_client.clone();
            let pod = pod.clone();

            {
                let mut set = pod_in_flight.lock().unwrap();
                if set.contains(&pod.id) {
                    continue;
                }
                set.insert(pod.id.clone());
            }

            tokio::spawn(async move {
                run_pod_lifecycle(
                    pod,
                    pod_runtime,
                    pod_client,
                    pod_server,
                    pod_token,
                    pod_in_flight,
                    pod_vpc,
                )
                .await;
            });
        }
    }
}

/// Full pod lifecycle: allocate VPC → pull image → create container → start → report.
async fn run_pod_lifecycle(
    pod: pkg_types::pod::Pod,
    runtime: Arc<ContainerRuntime>,
    client: reqwest::Client,
    server: String,
    token: String,
    in_flight: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    vpc_client: Arc<VpcClient>,
) {
    let status_url = format!(
        "{}/api/v1/namespaces/{}/pods/{}/status",
        server.trim_end_matches('/'),
        pod.namespace,
        pod.name
    );

    let container_spec = pod.spec.containers.first();
    let image = container_spec
        .map(|c| c.image.clone())
        .unwrap_or_else(|| "alpine:latest".to_string());
    let command: Vec<String> = container_spec
        .map(|c| {
            let mut cmd = c.command.clone();
            cmd.extend(c.args.clone());
            cmd
        })
        .unwrap_or_default();
    let env = container_spec.map(|c| c.env.clone()).unwrap_or_default();

    // 0. Allocate VPC address
    let vpc_name = pod.spec.vpc.as_deref().unwrap_or("default");
    let vpc_alloc = match vpc_client.allocate(&pod.id, vpc_name).await {
        Ok(alloc) => {
            info!(
                "[pod:{}] VPC allocated: ghost_ipv6={}, guest_ipv4={}, vpc_id={}",
                pod.name, alloc.1, alloc.0, alloc.2
            );
            Some(alloc)
        }
        Err(e) => {
            error!("[pod:{}] VPC allocation failed: {}", pod.name, e);
            let _ = client
                .put(&status_url)
                .header("Authorization", format!("Bearer {}", token))
                .json(&pkg_types::pod::PodStatus::Failed)
                .send()
                .await;
            in_flight.lock().unwrap().remove(&pod.id);
            return;
        }
    };

    // 1. Pull Image
    info!("[pod:{}] Pulling image: {}", pod.name, image);
    if let Err(e) = runtime.pull_image(&image).await {
        error!("[pod:{}] Image pull failed: {}", pod.name, e);
        let _ = client
            .put(&status_url)
            .header("Authorization", format!("Bearer {}", token))
            .json(&pkg_types::pod::PodStatus::Failed)
            .send()
            .await;
        return;
    }

    // 2. Create Container
    info!("[pod:{}] Creating container: {}", pod.name, pod.id);
    if let Err(e) = runtime
        .create_container(&pod.id, &image, &command, &env, pod.spec.runtime.as_deref())
        .await
    {
        error!("[pod:{}] Container creation failed: {}", pod.name, e);
        let _ = client
            .put(&status_url)
            .header("Authorization", format!("Bearer {}", token))
            .json(&pkg_types::pod::PodStatus::Failed)
            .send()
            .await;
        return;
    }

    // 3. Start Container
    info!("[pod:{}] Starting container: {}", pod.name, pod.id);
    if let Err(e) = runtime.start_container(&pod.id).await {
        error!("[pod:{}] Container start failed: {}", pod.name, e);
        in_flight.lock().unwrap().remove(&pod.id);
        let _ = runtime.cleanup_container(&pod.id).await;
        let _ = client
            .put(&status_url)
            .header("Authorization", format!("Bearer {}", token))
            .json(&pkg_types::pod::PodStatus::Failed)
            .send()
            .await;
        return;
    }

    // 4. Success
    in_flight.lock().unwrap().remove(&pod.id);
    info!(
        "[pod:{}] Container running via {}",
        pod.name,
        runtime.backend_name_for(&pod.id)
    );
    let _ = client
        .put(&status_url)
        .header("Authorization", format!("Bearer {}", token))
        .json(&pkg_types::pod::PodStatus::Running)
        .send()
        .await;

    // 5. Report VPC info to server (best-effort)
    if let Some((_, ref ghost_ipv6, _)) = vpc_alloc {
        let vpc_url = format!(
            "{}/api/v1/namespaces/{}/pods/{}/vpc",
            server.trim_end_matches('/'),
            pod.namespace,
            pod.name
        );
        let vpc_body = serde_json::json!({
            "ghost_ipv6": ghost_ipv6,
            "vpc_name": vpc_name,
        });
        if let Err(e) = client
            .put(&vpc_url)
            .header("Authorization", format!("Bearer {}", token))
            .json(&vpc_body)
            .send()
            .await
        {
            warn!("[pod:{}] Failed to report VPC info: {}", pod.name, e);
        }
    }
}
