//! AgentStateCache — in-memory representation of Agent state.
//!
//! Loaded from `AgentStore` on startup, written back after every successful
//! server sync. `AgentStore` (SlateDB) owns persistence; this struct is
//! purely the in-memory view passed through `Arc<RwLock<AgentStateCache>>`.

use chrono::{DateTime, Utc};
use pkg_types::{endpoint::Endpoint, ingress::Ingress, pod::Pod, service::Service};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// In-memory representation of Agent state.
/// Persisted to AgentStore (SlateDB) after every successful server sync.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStateCache {
    pub node_name: String,
    /// Remembered from registration so sync loops work offline.
    #[serde(default)]
    pub node_id: Option<String>,
    /// Remembered from registration for agent API server.
    #[serde(default)]
    pub agent_api_port: Option<u16>,
    /// Monotonic sequence from server EventLog.
    pub server_seq: u64,
    /// Timestamp of last successful server sync.
    pub last_synced_at: DateTime<Utc>,
    /// Desired pod specs for this node.
    pub pods: Vec<Pod>,
    /// All Services across all namespaces.
    pub services: Vec<Service>,
    /// All Endpoints across all namespaces.
    pub endpoints: Vec<Endpoint>,
    /// All Ingress rules.
    pub ingresses: Vec<Ingress>,
}

impl AgentStateCache {
    /// Create a new empty cache for the given node name.
    pub fn new(node_name: String) -> Self {
        Self {
            node_name,
            node_id: None,
            agent_api_port: None,
            server_seq: 0,
            last_synced_at: Utc::now(),
            pods: Vec::new(),
            services: Vec::new(),
            endpoints: Vec::new(),
            ingresses: Vec::new(),
        }
    }

    /// Seconds since last successful sync.
    pub fn age_secs(&self) -> i64 {
        Utc::now()
            .signed_duration_since(self.last_synced_at)
            .num_seconds()
    }

    /// Derive routing table from cached services + endpoints.
    /// Returns `HashMap<"ClusterIP:port", Vec<"backendIP:targetPort">>`.
    ///
    /// This is a pure computation — no I/O. Called by `AgentStore::save()`
    /// to write the derived `/agent/routes` key in the same `WriteBatch`.
    pub fn derive_routes_map(&self) -> HashMap<String, Vec<String>> {
        let mut routes: HashMap<String, Vec<String>> = HashMap::new();

        for svc in &self.services {
            let cluster_ip = match &svc.cluster_ip {
                Some(ip) => ip.clone(),
                None => continue,
            };

            let matching_eps: Vec<&Endpoint> = self
                .endpoints
                .iter()
                .filter(|ep| ep.service_id == svc.id && ep.namespace == svc.namespace)
                .collect();

            for svc_port in &svc.spec.ports {
                let route_key = format!("{}:{}", cluster_ip, svc_port.port);
                let mut backends = Vec::new();

                for ep in &matching_eps {
                    for addr in &ep.addresses {
                        backends.push(format!("{}:{}", addr.ip, svc_port.target_port));
                    }
                }

                if !backends.is_empty() {
                    routes.insert(route_key, backends);
                }
            }
        }

        routes
    }

    /// Derive DNS records from cached services.
    /// Returns `HashMap<"name.ns.svc.cluster.local", "ClusterIP">`.
    ///
    /// This is a pure computation — no I/O. Called by `AgentStore::save()`
    /// to write the derived `/agent/dns-records` key in the same `WriteBatch`.
    pub fn derive_dns_map(&self) -> HashMap<String, String> {
        let domain_suffix = pkg_constants::dns::DNS_DOMAIN_SUFFIX;
        let mut records: HashMap<String, String> = HashMap::new();

        for svc in &self.services {
            if let Some(ref cluster_ip) = svc.cluster_ip {
                let fqdn = format!("{}.{}.{}", svc.name, svc.namespace, domain_suffix);
                records.insert(fqdn, cluster_ip.clone());
            }
        }

        records
    }
}
