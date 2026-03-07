use crate::cache::AgentStateCache;
use crate::connectivity::ConnectivityManager;
use crate::store::AgentStore;
use crate::vpc_client::VpcClient;
use pkg_container::ContainerRuntime;
use pkg_network::dns::DnsServer;
use pkg_proxy::service_proxy::ServiceProxy;
use pkg_types::node::NodeRegistrationRequest;
use std::sync::Arc;
use tracing::{info, warn};

pub mod image_report;
pub mod pod_sync;
pub mod reconnect;
pub mod route_sync;

/// Start all controller loops on a dedicated OS thread with its own multi-threaded runtime.
#[allow(clippy::too_many_arguments)]
pub fn start_controller_loops(
    server: String,
    token: String,
    service_proxy: Arc<ServiceProxy>,
    dns_server: Arc<DnsServer>,
    cache: Arc<std::sync::RwLock<AgentStateCache>>,
    connectivity: Arc<ConnectivityManager>,
    reg_req: NodeRegistrationRequest,
    node_name: String,
    store: AgentStore,
    vpc_client: Arc<VpcClient>,
) {
    info!("Starting node controllers (pod-sync, image-report, route-sync)");

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("controller runtime");

        rt.block_on(async move {
            let client = reqwest::Client::builder()
                .danger_accept_invalid_certs(true)
                .build()
                .unwrap();

            // Read node_id and agent_api_port from cache (may be None if never registered)
            let (initial_node_id, initial_api_port) = {
                let c = cache.read().unwrap();
                (c.node_id.clone(), c.agent_api_port)
            };

            // Init container runtime (may download youki/crun)
            let runtime: Option<Arc<ContainerRuntime>> =
                match ContainerRuntime::new(None::<&str>).await {
                    Ok(rt) => {
                        let rt_arc = Arc::new(rt);
                        info!("Container runtime ready: {}", rt_arc.backend_name());

                        // Initialize k3rs0 dummy device for DNS VIP + routing anchor (Linux only, non-fatal)
                        #[cfg(target_os = "linux")]
                        {
                            let bridge_config = pkg_network::linux::bridge::BridgeConfig::default();
                            if let Err(e) = pkg_network::linux::bridge::ensure_bridge(&bridge_config).await
                            {
                                warn!(
                                    "Failed to create k3rs0 device: {} (pod networking may be unavailable)",
                                    e
                                );
                            }

                            // Start DNS listener on bridge VIP (port 53) so pods can resolve
                            let dns_vip_addr: std::net::SocketAddr =
                                format!("[{}]:53", pkg_constants::network::DNS_VIP)
                                    .parse()
                                    .expect("invalid DNS_VIP");
                            if let Err(e) = dns_server.start_on(dns_vip_addr).await {
                                warn!(
                                    "DNS VIP listener on {} failed: {} (pod DNS may not work)",
                                    dns_vip_addr, e
                                );
                            }
                        }

                        // Start Agent API server for exec/logs (if we know the port)
                        if let Some(api_port) = initial_api_port {
                            let agent_state = crate::api::AgentState {
                                runtime: rt_arc.clone(),
                            };
                            let agent_router = crate::api::create_agent_router(agent_state);
                            let listener =
                                tokio::net::TcpListener::bind(format!("0.0.0.0:{}", api_port))
                                    .await
                                    .expect("Failed to bind agent API port");
                            info!("Agent API listening on 0.0.0.0:{}", api_port);
                            tokio::spawn(async move {
                                axum::serve(listener, agent_router).await.ok();
                            });
                        }

                        // --- Agent Recovery ---
                        let cached_pods = cache.read().unwrap().pods.clone();
                        crate::recovery::run_recovery(
                            &rt_arc,
                            initial_node_id.as_deref(),
                            &server,
                            &node_name,
                            &token,
                            &client,
                            cached_pods,
                        )
                        .await;

                        Some(rt_arc)
                    }
                    Err(e) => {
                        warn!("Container runtime not available: {}. Pods will fail.", e);
                        None
                    }
                };

            // Start all sub-loops
            image_report::start(
                runtime.clone(),
                client.clone(),
                server.clone(),
                token.clone(),
                cache.clone(),
                connectivity.clone(),
            );

            reconnect::start(
                client.clone(),
                server.clone(),
                token.clone(),
                reg_req,
                node_name.clone(),
                cache.clone(),
                connectivity.clone(),
                store.clone(),
            );

            // macOS: create userspace switch for VM VPC networking
            #[cfg(target_os = "macos")]
            let mac_switch: Option<Arc<pkg_network::macos::switch::MacSwitch>> = {
                let switch = Arc::new(pkg_network::macos::switch::MacSwitch::new(53));
                switch.clone().start();
                info!("macOS userspace switch started");
                Some(switch)
            };

            pod_sync::start(
                runtime,
                client.clone(),
                server.clone(),
                token.clone(),
                node_name.clone(),
                cache.clone(),
                connectivity.clone(),
                store.clone(),
                vpc_client.clone(),
                #[cfg(target_os = "macos")]
                mac_switch,
            );

            route_sync::start(
                client.clone(),
                server.clone(),
                token.clone(),
                service_proxy,
                dns_server,
                cache.clone(),
                connectivity.clone(),
                store.clone(),
                vpc_client,
            );

            // Keep this thread alive forever
            tokio::signal::ctrl_c().await.ok();
        });
    });
}
