//! AgentStore — embedded SlateDB instance for Agent local state.
//!
//! Replaces the ad-hoc JSON-file approach (state.json / routes.json / dns-records.json)
//! with a proper KV database: atomic WriteBatch writes, WAL crash-safety, MVCC reads.
//!
//! # Key schema
//!
//! Each collection is stored as a **single JSON-array value** under a fixed key.
//! This avoids stale-key accumulation from per-object keys and matches the
//! "always full-overwrite on re-sync" semantics exactly.
//!
//! ```text
//! /agent/meta          → AgentMeta JSON  (node_id, node_name, agent_api_port, server_seq, last_synced_at)
//! /agent/pods          → Vec<Pod> JSON array
//! /agent/services      → Vec<Service> JSON array
//! /agent/endpoints     → Vec<Endpoint> JSON array
//! /agent/ingresses     → Vec<Ingress> JSON array
//! /agent/routes        → HashMap<String,Vec<String>> JSON  (derived: ClusterIP:port → backends)
//! /agent/dns-records   → HashMap<String,String> JSON       (derived: FQDN → ClusterIP)
//! ```
//!
//! All 7 keys are written in a single `WriteBatch` — either all commit or none do.

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

// ---- Key constants (fixed, one key per collection) ----
const KEY_META: &[u8] = b"/agent/meta";
const KEY_PODS: &[u8] = b"/agent/pods";
const KEY_SERVICES: &[u8] = b"/agent/services";
const KEY_ENDPOINTS: &[u8] = b"/agent/endpoints";
const KEY_INGRESSES: &[u8] = b"/agent/ingresses";
const KEY_ROUTES: &[u8] = b"/agent/routes";
const KEY_DNS: &[u8] = b"/agent/dns-records";

/// Identity fields stored under `/agent/meta`.
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
/// shared across tokio runtimes / threads without wrapping in `Arc`.
#[derive(Clone)]
pub struct AgentStore {
    db: Db,
}

impl AgentStore {
    /// Open (or create) the SlateDB at `<data_dir>/agent/state.db/`.
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
    /// Each collection is stored as one JSON-array value. Because we always
    /// overwrite the entire array, stale entries from previous syncs are
    /// automatically replaced — no per-key deletion logic required.
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
        batch.put(KEY_META, serde_json::to_vec(&meta)?);
        batch.put(KEY_PODS, serde_json::to_vec(&cache.pods)?);
        batch.put(KEY_SERVICES, serde_json::to_vec(&cache.services)?);
        batch.put(KEY_ENDPOINTS, serde_json::to_vec(&cache.endpoints)?);
        batch.put(KEY_INGRESSES, serde_json::to_vec(&cache.ingresses)?);
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
        // Fast fresh-node check: if meta is missing, nothing was ever saved.
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

        let pods: Vec<Pod> = self.get_collection(KEY_PODS).await?;
        let services: Vec<Service> = self.get_collection(KEY_SERVICES).await?;
        let endpoints: Vec<Endpoint> = self.get_collection(KEY_ENDPOINTS).await?;
        let ingresses: Vec<Ingress> = self.get_collection(KEY_INGRESSES).await?;

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

    /// Read the pre-computed routing table for fast `ServiceProxy` bootstrap.
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

    /// Read the pre-computed DNS record map for fast `DnsServer` bootstrap.
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

    /// Get a collection stored as a JSON array. Returns an empty Vec if the
    /// key does not exist (e.g. the field was never written on an older schema).
    async fn get_collection<T: serde::de::DeserializeOwned>(
        &self,
        key: &[u8],
    ) -> anyhow::Result<Vec<T>> {
        match self
            .db
            .get(key)
            .await
            .map_err(|e| anyhow::anyhow!("AgentStore get {:?}: {}", String::from_utf8_lossy(key), e))?
        {
            Some(b) => Ok(serde_json::from_slice(&b)?),
            None => Ok(Vec::new()),
        }
    }
}
