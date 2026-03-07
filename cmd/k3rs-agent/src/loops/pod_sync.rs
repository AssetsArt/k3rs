use crate::cache::AgentStateCache;
use crate::connectivity::ConnectivityManager;
use crate::store::AgentStore;
use crate::vpc_client::VpcClient;
use chrono::Utc;
use pkg_container::ContainerRuntime;
#[cfg(target_os = "macos")]
use pkg_network::macos::switch::MacSwitch;
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
    #[cfg(target_os = "macos")] mac_switch: Option<Arc<MacSwitch>>,
) {
    let in_flight: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(
            pkg_constants::timings::POD_SYNC_INTERVAL_SECS,
        ));
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
                        check_running_pods(
                            &pods,
                            runtime,
                            &client,
                            &server,
                            &token,
                            &vpc_client,
                            #[cfg(target_os = "macos")]
                            &mac_switch,
                        )
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
                        #[cfg(target_os = "macos")]
                        &mac_switch,
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
    #[cfg(target_os = "macos")] mac_switch: &Option<Arc<MacSwitch>>,
) {
    for pod in pods
        .iter()
        .filter(|p| p.status == pkg_types::pod::PodStatus::Running)
    {
        match runtime.container_state(&pod.id).await {
            Ok(state) if state.status == "stopped" || state.status == "exited" => {
                warn!("[pod:{}] Container stopped unexpectedly", pod.name);

                // Unregister VM from userspace switch (macOS only)
                #[cfg(target_os = "macos")]
                if let Some(switch) = mac_switch {
                    switch.remove_vm(&pod.id).await;
                }

                // Detach eBPF classifiers (best-effort)
                #[cfg(target_os = "linux")]
                {
                    let short = &pod.id[..8.min(pod.id.len())];
                    if runtime.backend_name_for(&pod.id) == "vm" {
                        let tap_name = format!("tap-{}", short);
                        let _ = vpc_client.detach_tap(&tap_name).await;
                    } else {
                        let nk_name = format!("nk-{}", short);
                        let _ = vpc_client.detach_netkit(&nk_name).await;
                    }
                }

                // Tear down pod network (best-effort)
                #[cfg(target_os = "linux")]
                pkg_network::linux::netns::teardown_pod_network(&pod.id, None).await;

                // Release VPC allocation (best-effort)
                let vpc_name = pod
                    .vpc_name
                    .as_deref()
                    .or(pod.spec.vpc.as_deref())
                    .unwrap_or(pkg_constants::network::DEFAULT_VPC_NAME);
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

                // Unregister VM from userspace switch (macOS only)
                #[cfg(target_os = "macos")]
                if let Some(switch) = mac_switch {
                    switch.remove_vm(&pod.id).await;
                }

                // Detach eBPF classifiers (best-effort)
                #[cfg(target_os = "linux")]
                {
                    let short = &pod.id[..8.min(pod.id.len())];
                    if runtime.backend_name_for(&pod.id) == "vm" {
                        let tap_name = format!("tap-{}", short);
                        let _ = vpc_client.detach_tap(&tap_name).await;
                    } else {
                        let nk_name = format!("nk-{}", short);
                        let _ = vpc_client.detach_netkit(&nk_name).await;
                    }
                }

                // Tear down pod network (best-effort)
                #[cfg(target_os = "linux")]
                pkg_network::linux::netns::teardown_pod_network(&pod.id, None).await;

                // Release VPC allocation (best-effort)
                let vpc_name = pod
                    .vpc_name
                    .as_deref()
                    .or(pod.spec.vpc.as_deref())
                    .unwrap_or(pkg_constants::network::DEFAULT_VPC_NAME);
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
#[allow(clippy::too_many_arguments)]
fn schedule_new_pods(
    pods: &[pkg_types::pod::Pod],
    runtime: &Option<Arc<ContainerRuntime>>,
    client: &reqwest::Client,
    server: &str,
    token: &str,
    in_flight: &std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    vpc_client: &Arc<VpcClient>,
    #[cfg(target_os = "macos")] mac_switch: &Option<Arc<MacSwitch>>,
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
            #[cfg(target_os = "macos")]
            let pod_switch = mac_switch.clone();

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
                    #[cfg(target_os = "macos")]
                    pod_switch,
                )
                .await;
            });
        }
    }
}

/// Full pod lifecycle: allocate VPC → pull image → create container → start → report.
#[allow(clippy::too_many_arguments)]
async fn run_pod_lifecycle(
    pod: pkg_types::pod::Pod,
    runtime: Arc<ContainerRuntime>,
    client: reqwest::Client,
    server: String,
    token: String,
    in_flight: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    vpc_client: Arc<VpcClient>,
    #[cfg(target_os = "macos")] mac_switch: Option<Arc<MacSwitch>>,
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
    let mut env = container_spec.map(|c| c.env.clone()).unwrap_or_default();

    // 0. Allocate VPC address
    let vpc_name = pod
        .spec
        .vpc
        .as_deref()
        .unwrap_or(pkg_constants::network::DEFAULT_VPC_NAME);
    let vpc_alloc = match vpc_client.allocate(&pod.id, vpc_name).await {
        Ok(alloc) => {
            info!(
                "[pod:{}] VPC allocated: ghost_ipv6={}, guest_ipv4={}, vpc_id={}",
                pod.name, alloc.1, alloc.0, alloc.2
            );
            Some(alloc)
        }
        Err(e) => {
            // If VPC daemon is not running (socket missing), keep pod Pending
            // so it will be retried on the next sync cycle.
            let msg = e.to_string();
            let is_transient = msg.contains("os error 2")
                || msg.contains("Connection refused")
                || msg.contains("socket not found");
            if is_transient {
                warn!(
                    "[pod:{}] VPC daemon not available, will retry: {}",
                    pod.name, e
                );
            } else {
                error!("[pod:{}] VPC allocation failed: {}", pod.name, e);
                let _ = client
                    .put(&status_url)
                    .header("Authorization", format!("Bearer {}", token))
                    .json(&pkg_types::pod::PodStatus::Failed)
                    .send()
                    .await;
            }
            in_flight.lock().unwrap().remove(&pod.id);
            return;
        }
    };

    // Inject VPC addresses as environment variables
    if let Some((ref guest_ipv4, ref ghost_ipv6, _, _)) = vpc_alloc {
        env.insert("K3RS_POD_IP".to_string(), guest_ipv4.clone());
        env.insert("K3RS_POD_IPV6".to_string(), ghost_ipv6.clone());
    }

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

    // 2b. Pod network setup (netkit pair + Ghost IPv6) — skip for VM backends
    #[cfg(target_os = "linux")]
    if runtime.backend_name_for(&pod.id) != "vm"
        && let Some((ref guest_ipv4, ref ghost_ipv6, vpc_id, ref vpc_cidr)) = vpc_alloc
    {
        if let Some(pid) = runtime.container_pid(&pod.id) {
            let net_config = pkg_network::linux::netns::PodNetworkConfig {
                pod_id: pod.id.clone(),
                ghost_ipv6: ghost_ipv6.clone(),
                guest_ipv4: guest_ipv4.clone(),
                container_pid: pid,
            };
            if let Err(e) = pkg_network::linux::netns::setup_pod_network(&net_config).await {
                warn!(
                    "[pod:{}] Pod network setup failed: {} (continuing without network)",
                    pod.name, e
                );
            }

            // 2c. Attach eBPF SIIT + VPC isolation classifiers via k3rs-vpc
            let short = &pod.id[..8.min(pod.id.len())];
            let nk_name = format!("nk-{}", short);
            if let Err(e) = vpc_client
                .attach_netkit(&nk_name, guest_ipv4, ghost_ipv6, vpc_id, vpc_cidr, pid)
                .await
            {
                warn!(
                    "[pod:{}] eBPF attach_netkit failed: {} (continuing without SIIT)",
                    pod.name, e
                );
            }
        } else {
            warn!(
                "[pod:{}] Container PID not available, skipping network setup",
                pod.name
            );
        }
    }

    // 2d. Set VPC config on VM backend before start (macOS only)
    // This creates a socketpair and passes VPC params to the VMM process.
    #[cfg(target_os = "macos")]
    if runtime.backend_name_for(&pod.id) == "vm"
        && let Some((ref guest_ipv4, ref ghost_ipv6, vpc_id, ref vpc_cidr)) = vpc_alloc
    {
        let vpc_config = pkg_container::vm_utils::VmNetworkConfig {
            guest_ipv4: guest_ipv4.clone(),
            guest_ipv6: ghost_ipv6.clone(),
            vpc_id,
            vpc_cidr: vpc_cidr.clone(),
            gw_mac: pkg_constants::network::GATEWAY_MAC_STR.to_string(),
            platform_prefix: pkg_constants::network::PLATFORM_PREFIX,
            cluster_id: 0, // TODO: get from cluster registration
        };
        runtime.set_vm_network_config(&pod.id, vpc_config).await;
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

    // 3b. Attach eBPF classifiers for VM backends (TAP created during start_container)
    #[cfg(target_os = "linux")]
    if runtime.backend_name_for(&pod.id) == "vm"
        && let Some((ref guest_ipv4, ref ghost_ipv6, vpc_id, ref vpc_cidr)) = vpc_alloc
    {
        let short = &pod.id[..8.min(pod.id.len())];
        let tap_name = format!("tap-{}", short);
        if let Err(e) = vpc_client
            .attach_tap(&tap_name, guest_ipv4, ghost_ipv6, vpc_id, vpc_cidr)
            .await
        {
            warn!(
                "[pod:{}] eBPF attach_tap failed: {} (continuing without VPC enforcement)",
                pod.name, e
            );
        }
    }

    // 3c. Register VM with userspace switch (macOS only)
    #[cfg(target_os = "macos")]
    if runtime.backend_name_for(&pod.id) == "vm"
        && let Some((ref guest_ipv4, _, vpc_id, _)) = vpc_alloc
        && let Some(switch) = &mac_switch
        && let Some(socket) = runtime.take_vm_net_socket(&pod.id).await
        && let Ok(ip) = guest_ipv4.parse()
        && let Err(e) = switch.add_vm(pod.id.clone(), socket, ip, vpc_id).await
    {
        warn!(
            "[pod:{}] Failed to register VM with switch: {}",
            pod.name, e
        );
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
    if let Some((_, ref ghost_ipv6, _, _)) = vpc_alloc {
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
