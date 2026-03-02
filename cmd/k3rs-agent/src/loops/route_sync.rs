use crate::cache::AgentStateCache;
use crate::connectivity::ConnectivityManager;
use crate::store::AgentStore;
use crate::vpc_client::VpcClient;
use chrono::Utc;
use pkg_network::dns::DnsServer;
use pkg_proxy::service_proxy::ServiceProxy;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tracing::warn;

/// Start the route sync loop (every 10s).
pub fn start(
    client: reqwest::Client,
    server: String,
    token: String,
    service_proxy: Arc<ServiceProxy>,
    dns_server: Arc<DnsServer>,
    cache: Arc<std::sync::RwLock<AgentStateCache>>,
    connectivity: Arc<ConnectivityManager>,
    store: AgentStore,
    vpc_client: Arc<VpcClient>,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(10));
        loop {
            interval.tick().await;

            // Skip when not connected
            if !connectivity.is_connected() {
                continue;
            }

            let base = server.trim_end_matches('/');
            let auth = format!("Bearer {}", token);

            let namespaces: Vec<pkg_types::namespace::Namespace> = match client
                .get(format!("{}/api/v1/namespaces", base))
                .header("Authorization", &auth)
                .send()
                .await
            {
                Ok(r) => r.json().await.unwrap_or_default(),
                Err(e) => {
                    warn!("Route sync: failed to fetch namespaces: {}", e);
                    continue;
                }
            };

            let ns_names: Vec<String> = if namespaces.is_empty() {
                vec!["default".to_string()]
            } else {
                namespaces.iter().map(|n| n.name.clone()).collect()
            };

            let mut all_services = Vec::new();
            let mut all_endpoints = Vec::new();

            for ns in &ns_names {
                let services: Vec<pkg_types::service::Service> = match client
                    .get(format!("{}/api/v1/namespaces/{}/services", base, ns))
                    .header("Authorization", &auth)
                    .send()
                    .await
                {
                    Ok(r) => r.json().await.unwrap_or_default(),
                    Err(e) => {
                        warn!("Route sync: failed to fetch services for ns {}: {}", ns, e);
                        continue;
                    }
                };

                let endpoints: Vec<pkg_types::endpoint::Endpoint> = match client
                    .get(format!("{}/api/v1/namespaces/{}/endpoints", base, ns))
                    .header("Authorization", &auth)
                    .send()
                    .await
                {
                    Ok(r) => r.json().await.unwrap_or_default(),
                    Err(e) => {
                        warn!("Route sync: failed to fetch endpoints for ns {}: {}", ns, e);
                        continue;
                    }
                };

                all_services.extend(services);
                all_endpoints.extend(endpoints);
            }

            // Build VPC pod-IP maps from k3rs-vpc daemon
            let mut vpc_pod_ips: HashMap<String, HashSet<String>> = HashMap::new();
            let mut ip_to_vpc: HashMap<String, String> = HashMap::new();

            if let Ok(vpcs) = vpc_client.list_vpcs().await {
                for vpc_info in &vpcs {
                    if let Ok(routes) = vpc_client.get_routes(vpc_info.vpc_id).await {
                        let mut ips = HashSet::new();
                        for entry in routes {
                            ip_to_vpc.insert(entry.destination.clone(), vpc_info.name.clone());
                            ips.insert(entry.destination);
                        }
                        vpc_pod_ips.insert(vpc_info.name.clone(), ips);
                    }
                }
            } else {
                warn!("Route sync: failed to list VPCs from k3rs-vpc, using unscoped fallback");
            }

            // Fetch VPC peerings for cross-VPC DNS resolution
            let peerings: Vec<pkg_types::vpc::VpcPeering> = match client
                .get(format!("{}/api/v1/vpc-peerings", base))
                .header("Authorization", &auth)
                .send()
                .await
            {
                Ok(r) => r.json().await.unwrap_or_default(),
                Err(e) => {
                    warn!("Route sync: failed to fetch VPC peerings: {}", e);
                    Vec::new()
                }
            };

            // Update in-memory routing + DNS (live, in-memory)
            service_proxy
                .update_routes(&all_services, &all_endpoints, &vpc_pod_ips)
                .await;
            dns_server.update_records(&all_services).await;
            dns_server
                .update_records_vpc(&all_services, &ip_to_vpc)
                .await;
            dns_server.update_peerings(&peerings).await;

            // Persist to AgentStore (single WriteBatch: meta + services +
            // endpoints + derived /agent/routes + /agent/dns-records)
            {
                let mut c = cache.write().unwrap();
                c.services = all_services;
                c.endpoints = all_endpoints;
                c.last_synced_at = Utc::now();
            }
            let snapshot = cache.read().unwrap().clone();
            if let Err(e) = store.save(&snapshot).await {
                warn!("Failed to save to AgentStore after route sync: {}", e);
            }
        }
    });
}
