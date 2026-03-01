use chrono::{DateTime, Utc};
use pkg_types::{endpoint::Endpoint, ingress::Ingress, pod::Pod, service::Service};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use tracing::info;

/// Persisted to `<DATA_DIR>/agent/state.json` after every successful server sync.
/// Single source of truth for offline operation.
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

fn cache_dir() -> String {
    format!("{}/agent", pkg_constants::paths::DATA_DIR)
}

pub fn state_path() -> String {
    format!("{}/state.json", cache_dir())
}

pub fn routes_path() -> String {
    format!("{}/routes.json", cache_dir())
}

pub fn dns_path() -> String {
    format!("{}/dns-records.json", cache_dir())
}

/// Write data to a file atomically: write to .tmp → fsync → rename.
fn atomic_write(path: &str, data: &[u8]) -> anyhow::Result<()> {
    let tmp_path = format!("{}.tmp", path);

    if let Some(parent) = Path::new(path).parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut file = std::fs::File::create(&tmp_path)?;
    file.write_all(data)?;
    file.sync_all()?;

    std::fs::rename(&tmp_path, path)?;
    Ok(())
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

    /// Atomically write the full state to state.json.
    pub fn save(&self) -> anyhow::Result<()> {
        let data = serde_json::to_vec_pretty(self)?;
        atomic_write(&state_path(), &data)?;
        info!(
            "Cache saved: {} pods, {} services, {} endpoints",
            self.pods.len(),
            self.services.len(),
            self.endpoints.len()
        );
        Ok(())
    }

    /// Load state from state.json. Returns None if file does not exist.
    pub fn load() -> anyhow::Result<Option<Self>> {
        let path = state_path();
        let data = match std::fs::read_to_string(&path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let cache: Self = serde_json::from_str(&data)?;
        info!(
            "Cache loaded: {} pods, {} services, {} endpoints (last synced: {})",
            cache.pods.len(),
            cache.services.len(),
            cache.endpoints.len(),
            cache.last_synced_at,
        );
        Ok(Some(cache))
    }

    /// Derive routing table from cached services and endpoints,
    /// matching the logic in `ServiceProxy::update_routes()`.
    /// Writes routes.json atomically and returns the routes HashMap.
    pub fn derive_routes(&self) -> anyhow::Result<HashMap<String, Vec<String>>> {
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

        let data = serde_json::to_vec_pretty(&routes)?;
        atomic_write(&routes_path(), &data)?;
        info!("Derived routes.json: {} routes", routes.len());
        Ok(routes)
    }

    /// Derive DNS records from cached services,
    /// matching the logic in `DnsServer::update_records()`.
    /// Writes dns-records.json atomically and returns the records HashMap.
    pub fn derive_dns(&self) -> anyhow::Result<HashMap<String, String>> {
        let domain_suffix = "svc.cluster.local";
        let mut records: HashMap<String, String> = HashMap::new();

        for svc in &self.services {
            if let Some(ref cluster_ip) = svc.cluster_ip {
                let fqdn = format!("{}.{}.{}", svc.name, svc.namespace, domain_suffix);
                records.insert(fqdn, cluster_ip.clone());
            }
        }

        let data = serde_json::to_vec_pretty(&records)?;
        atomic_write(&dns_path(), &data)?;
        info!("Derived dns-records.json: {} records", records.len());
        Ok(records)
    }
}
