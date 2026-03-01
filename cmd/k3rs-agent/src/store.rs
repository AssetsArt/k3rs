//! AgentStore — embedded SlateDB instance for Agent local state.
//!
//! Replaces the ad-hoc JSON-file approach (state.json / routes.json / dns-records.json)
//! with a proper KV database: atomic WriteBatch writes, WAL crash-safety, MVCC reads.
//!
//! # Key schema
//! ```
//! /agent/meta                        → AgentMeta JSON (identity fields)
//! /agent/pods/<id>                   → Pod JSON
//! /agent/services/<ns>/<name>        → Service JSON
//! /agent/endpoints/<id>              → Endpoint JSON
//! /agent/ingresses/<ns>/<name>       → Ingress JSON
//! /agent/routes                      → HashMap<String,Vec<String>> JSON (derived: ClusterIP:port → backends)
//! /agent/dns-records                 → HashMap<String,String> JSON (derived: FQDN → ClusterIP)
//! ```

use anyhow::Context;
use chrono::{DateTime, Utc};
use pkg_types::{endpoint::Endpoint, ingress::Ingress, pod::Pod, service::Service};
use serde::{Deserialize, Serialize};
use slatedb::{Db, WriteBatch};
use slatedb::object_store::local::LocalFileSystem;
use slatedb::object_store::path::Path;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::info;

use crate::cache::AgentStateCache;

// ---- Key constants ----
const KEY_META: &[u8] = b"/agent/meta";
const KEY_ROUTES: &[u8] = b"/agent/routes";
const KEY_DNS: &[u8] = b"/agent/dns-records";

const PREFIX_PODS: &str = "/agent/pods/";
const PREFIX_SERVICES: &str = "/agent/services/";
const PREFIX_ENDPOINTS: &str = "/agent/endpoints/";
const PREFIX_INGRESSES: &str = "/agent/ingresses/";

fn key_pod(id: &str) -> Vec<u8> {
    format!("/agent/pods/{}", id).into_bytes()
}
fn key_service(ns: &str, name: &str) -> Vec<u8> {
    format!("/agent/services/{}/{}", ns, name).into_bytes()
}
fn key_endpoint(id: &str) -> Vec<u8> {
    format!("/agent/endpoints/{}", id).into_bytes()
}
fn key_ingress(ns: &str, name: &str) -> Vec<u8> {
    format!("/agent/ingresses/{}/{}", ns, name).into_bytes()
}

/// Minimal identity fields stored under `/agent/meta`.
/// Separate from the object lists so a fresh agent can read its node_id
/// without scanning all pods/services.
#[derive(Serialize, Deserialize)]
struct AgentMeta {
    node_name: String,
    node_id: Option<String>,
    agent_api_port: Option<u16>,
    server_seq: u64,
    last_synced_at: DateTime<Utc>,
}

/// Thin wrapper around a local SlateDB instance.
///
/// `AgentStore` is cheaply cloneable (internally Arc-backed) and can be
/// shared across tokio runtimes / threads without wrapping in Arc.
#[derive(Clone)]
pub struct AgentStore {
    db: Db,
}

impl AgentStore {
    /// Open (or create) the SlateDB at `<data_dir>/agent/state.db/`.
    /// Creates the directory if it does not exist.
    pub async fn open(data_dir: &str) -> anyhow::Result<Self> {
        let path = format!("{}/agent/state.db", data_dir);
        info!("Opening agent SlateDB at {}", path);

        std::fs::create_dir_all(&path)
            .with_context(|| format!("Failed to create agent state dir: {}", path))?;

        let object_store = Arc::new(
            LocalFileSystem::new_with_prefix(&path)
                .map_err(|e| anyhow::anyhow!("LocalFileSystem error at {}: {}", path, e))?,
        );
        let db = Db::open(Path::from("/"), object_store)
            .await
            .map_err(|e| anyhow::anyhow!("SlateDB open failed at {}: {}", path, e))?;

        Ok(Self { db })
    }

    /// Atomically save the full `AgentStateCache` in a single `WriteBatch`.
    ///
    /// Writes:
    /// - `/agent/meta` — identity fields
    /// - `/agent/pods/<id>` for every pod
    /// - `/agent/services/<ns>/<name>` for every service
    /// - `/agent/endpoints/<id>` for every endpoint
    /// - `/agent/ingresses/<ns>/<name>` for every ingress
    /// - `/agent/routes` — derived routing table (ClusterIP:port → backends)
    /// - `/agent/dns-records` — derived DNS map (FQDN → ClusterIP)
    pub async fn save(&self, cache: &AgentStateCache) -> anyhow::Result<()> {
        let meta = AgentMeta {
            node_name: cache.node_name.clone(),
            node_id: cache.node_id.clone(),
            agent_api_port: cache.agent_api_port,
            server_seq: cache.server_seq,
            last_synced_at: cache.last_synced_at,
        };

        let routes = cache.derive_routes_map();
        let dns = cache.derive_dns_map();

        let mut batch = WriteBatch::new();

        // Identity
        batch.put(KEY_META, serde_json::to_vec(&meta)?);

        // Pods
        for pod in &cache.pods {
            batch.put(key_pod(&pod.id), serde_json::to_vec(pod)?);
        }

        // Services
        for svc in &cache.services {
            batch.put(
                key_service(&svc.namespace, &svc.name),
                serde_json::to_vec(svc)?,
            );
        }

        // Endpoints (keyed by id — unique across the cluster)
        for ep in &cache.endpoints {
            batch.put(key_endpoint(&ep.id), serde_json::to_vec(ep)?);
        }

        // Ingresses
        for ing in &cache.ingresses {
            batch.put(
                key_ingress(&ing.namespace, &ing.name),
                serde_json::to_vec(ing)?,
            );
        }

        // Derived views written in the same batch
        batch.put(KEY_ROUTES, serde_json::to_vec(&routes)?);
        batch.put(KEY_DNS, serde_json::to_vec(&dns)?);

        self.db
            .write(batch)
            .await
            .map_err(|e| anyhow::anyhow!("AgentStore WriteBatch failed: {}", e))?;

        info!(
            "AgentStore saved: {} pods, {} services, {} endpoints, {} routes, {} dns",
            cache.pods.len(),
            cache.services.len(),
            cache.endpoints.len(),
            routes.len(),
            dns.len(),
        );
        Ok(())
    }

    /// Load full `AgentStateCache` on startup.
    /// Returns `None` if the database is empty (fresh node — no `/agent/meta` key).
    pub async fn load(&self) -> anyhow::Result<Option<AgentStateCache>> {
        // Check for meta first — fast early-return for fresh nodes
        let meta_bytes = match self
            .db
            .get(KEY_META)
            .await
            .map_err(|e| anyhow::anyhow!("AgentStore get meta: {}", e))?
        {
            Some(b) => b,
            None => return Ok(None),
        };
        let meta: AgentMeta = serde_json::from_slice(&meta_bytes)?;

        // Scan object prefixes
        let pods = self.scan_prefix::<Pod>(PREFIX_PODS).await?;
        let services = self.scan_prefix::<Service>(PREFIX_SERVICES).await?;
        let endpoints = self.scan_prefix::<Endpoint>(PREFIX_ENDPOINTS).await?;
        let ingresses = self.scan_prefix::<Ingress>(PREFIX_INGRESSES).await?;

        let cache = AgentStateCache {
            node_name: meta.node_name,
            node_id: meta.node_id,
            agent_api_port: meta.agent_api_port,
            server_seq: meta.server_seq,
            last_synced_at: meta.last_synced_at,
            pods,
            services,
            endpoints,
            ingresses,
        };

        info!(
            "AgentStore loaded: {} pods, {} services, {} endpoints (last synced: {})",
            cache.pods.len(),
            cache.services.len(),
            cache.endpoints.len(),
            cache.last_synced_at,
        );
        Ok(Some(cache))
    }

    /// Read only the pre-computed routing table for fast `ServiceProxy` bootstrap.
    /// Avoids deserializing all pods/services/endpoints.
    #[allow(dead_code)]
    pub async fn load_routes(&self) -> anyhow::Result<Option<HashMap<String, Vec<String>>>> {
        match self
            .db
            .get(KEY_ROUTES)
            .await
            .map_err(|e| anyhow::anyhow!("AgentStore get routes: {}", e))?
        {
            Some(b) => Ok(Some(serde_json::from_slice(&b)?)),
            None => Ok(None),
        }
    }

    /// Read only the pre-computed DNS record map for fast `DnsServer` bootstrap.
    /// Avoids deserializing all pods/services/endpoints.
    #[allow(dead_code)]
    pub async fn load_dns_records(&self) -> anyhow::Result<Option<HashMap<String, String>>> {
        match self
            .db
            .get(KEY_DNS)
            .await
            .map_err(|e| anyhow::anyhow!("AgentStore get dns: {}", e))?
        {
            Some(b) => Ok(Some(serde_json::from_slice(&b)?)),
            None => Ok(None),
        }
    }

    /// Gracefully close the underlying SlateDB instance (flushes WAL).
    pub async fn close(self) -> anyhow::Result<()> {
        self.db
            .close()
            .await
            .map_err(|e| anyhow::anyhow!("AgentStore close failed: {}", e))
    }

    // ---- Private helpers ----

    /// Scan all keys under `prefix` and deserialize values as `T`.
    /// Logs a warning (and skips) on any deserialization errors.
    async fn scan_prefix<T: serde::de::DeserializeOwned>(
        &self,
        prefix: &str,
    ) -> anyhow::Result<Vec<T>> {
        let mut results = Vec::new();
        let mut iter = self
            .db
            .scan_prefix(prefix.as_bytes())
            .await
            .map_err(|e| anyhow::anyhow!("scan_prefix '{}': {}", prefix, e))?;

        while let Ok(Some(kv)) = iter.next().await {
            match serde_json::from_slice::<T>(&kv.value) {
                Ok(v) => results.push(v),
                Err(e) => tracing::warn!(
                    "AgentStore: failed to deserialize key '{}': {}",
                    String::from_utf8_lossy(&kv.key),
                    e
                ),
            }
        }
        Ok(results)
    }
}
