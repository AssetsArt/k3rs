//! VpcStore — embedded SlateDB instance for VPC daemon local state.
//!
//! # Key schema
//!
//! ```text
//! /vpc/meta                       → VpcDaemonMeta JSON
//! /vpc/definitions/<vpc-name>     → Vpc JSON (per-VPC keys)
//! /vpc/peerings/<peering-name>    → VpcPeering JSON
//! ```

use anyhow::Context;
use chrono::{DateTime, Utc};
use pkg_types::vpc::{Vpc, VpcPeering};
use serde::{Deserialize, Serialize};
use slatedb::Db;
use slatedb::object_store::local::LocalFileSystem;
use slatedb::object_store::path::Path;
use std::sync::Arc;
use tracing::info;

const KEY_META: &[u8] = b"/vpc/meta";
const KEY_NFT_SNAPSHOT: &[u8] = b"/vpc/nftables-snapshot";
const PREFIX_DEFINITIONS: &str = "/vpc/definitions/";
const PREFIX_PEERINGS: &str = "/vpc/peerings/";
const PREFIX_ALLOCATIONS: &str = "/vpc/allocations/";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredAllocation {
    pub pod_id: String,
    pub vpc_name: String,
    pub guest_ipv4: String,
    pub ghost_ipv6: String,
    pub vpc_id: u16,
    pub allocated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VpcDaemonMeta {
    pub cluster_id: Option<u32>,
    pub platform_prefix: u32,
    pub last_synced_at: DateTime<Utc>,
}

/// Thin wrapper around a local SlateDB instance for VPC state.
#[derive(Clone)]
pub struct VpcStore {
    db: Db,
}

impl VpcStore {
    /// Open (or create) the SlateDB at `<data_dir>/vpc-state.db/`.
    pub async fn open(data_dir: &str) -> anyhow::Result<Self> {
        let path = format!("{}/vpc-state.db", data_dir);
        info!("Opening VPC SlateDB at {}", path);

        std::fs::create_dir_all(&path)
            .with_context(|| format!("Failed to create VPC state dir: {}", path))?;

        let object_store = Arc::new(
            LocalFileSystem::new_with_prefix(&path)
                .map_err(|e| anyhow::anyhow!("LocalFileSystem error at {}: {}", path, e))?,
        );
        let db = Db::open(Path::from("/"), object_store)
            .await
            .map_err(|e| anyhow::anyhow!("SlateDB open failed at {}: {}", path, e))?;

        Ok(Self { db })
    }

    pub async fn save_meta(&self, meta: &VpcDaemonMeta) -> anyhow::Result<()> {
        self.db
            .put(KEY_META, serde_json::to_vec(meta)?)
            .await
            .map_err(|e| anyhow::anyhow!("VpcStore put meta: {}", e))
    }

    pub async fn load_meta(&self) -> anyhow::Result<Option<VpcDaemonMeta>> {
        match self
            .db
            .get(KEY_META)
            .await
            .map_err(|e| anyhow::anyhow!("VpcStore get meta: {}", e))?
        {
            Some(b) => Ok(Some(serde_json::from_slice(&b)?)),
            None => Ok(None),
        }
    }

    pub async fn save_vpcs(&self, vpcs: &[Vpc]) -> anyhow::Result<()> {
        // Write each VPC under its own key.
        // We store the full list as individual keys so we can scan the prefix.
        let mut batch = slatedb::WriteBatch::new();
        for vpc in vpcs {
            let key = format!("{}{}", PREFIX_DEFINITIONS, vpc.name);
            batch.put(key.as_bytes(), serde_json::to_vec(vpc)?);
        }
        self.db
            .write(batch)
            .await
            .map_err(|e| anyhow::anyhow!("VpcStore save_vpcs batch: {}", e))?;
        info!("VpcStore saved {} VPC definitions", vpcs.len());
        Ok(())
    }

    pub async fn load_vpcs(&self) -> anyhow::Result<Vec<Vpc>> {
        let prefix = PREFIX_DEFINITIONS.as_bytes();
        let mut vpcs = Vec::new();
        let mut iter = self
            .db
            .scan(prefix..)
            .await
            .map_err(|e| anyhow::anyhow!("VpcStore scan vpcs: {}", e))?;
        while let Some(kv) = iter
            .next()
            .await
            .map_err(|e| anyhow::anyhow!("VpcStore scan vpcs next: {}", e))?
        {
            if !kv.key.starts_with(prefix) {
                break;
            }
            let vpc: Vpc = serde_json::from_slice(&kv.value)?;
            vpcs.push(vpc);
        }
        Ok(vpcs)
    }

    pub async fn save_peerings(&self, peerings: &[VpcPeering]) -> anyhow::Result<()> {
        let mut batch = slatedb::WriteBatch::new();
        for peering in peerings {
            let key = format!("{}{}", PREFIX_PEERINGS, peering.name);
            batch.put(key.as_bytes(), serde_json::to_vec(peering)?);
        }
        self.db
            .write(batch)
            .await
            .map_err(|e| anyhow::anyhow!("VpcStore save_peerings batch: {}", e))?;
        info!("VpcStore saved {} VPC peerings", peerings.len());
        Ok(())
    }

    pub async fn load_peerings(&self) -> anyhow::Result<Vec<VpcPeering>> {
        let prefix = PREFIX_PEERINGS.as_bytes();
        let mut peerings = Vec::new();
        let mut iter = self
            .db
            .scan(prefix..)
            .await
            .map_err(|e| anyhow::anyhow!("VpcStore scan peerings: {}", e))?;
        while let Some(kv) = iter
            .next()
            .await
            .map_err(|e| anyhow::anyhow!("VpcStore scan peerings next: {}", e))?
        {
            if !kv.key.starts_with(prefix) {
                break;
            }
            let peering: VpcPeering = serde_json::from_slice(&kv.value)?;
            peerings.push(peering);
        }
        Ok(peerings)
    }

    pub async fn save_allocation(&self, alloc: &StoredAllocation) -> anyhow::Result<()> {
        let key = format!("{}{}/{}", PREFIX_ALLOCATIONS, alloc.vpc_name, alloc.pod_id);
        self.db
            .put(key.as_bytes(), serde_json::to_vec(alloc)?)
            .await
            .map_err(|e| anyhow::anyhow!("VpcStore save_allocation: {}", e))
    }

    pub async fn delete_allocation(&self, vpc_name: &str, pod_id: &str) -> anyhow::Result<()> {
        let key = format!("{}{}/{}", PREFIX_ALLOCATIONS, vpc_name, pod_id);
        self.db
            .delete(key.as_bytes())
            .await
            .map_err(|e| anyhow::anyhow!("VpcStore delete_allocation: {}", e))
    }

    pub async fn load_all_allocations(&self) -> anyhow::Result<Vec<StoredAllocation>> {
        let prefix = PREFIX_ALLOCATIONS.as_bytes();
        let mut allocs = Vec::new();
        let mut iter = self
            .db
            .scan(prefix..)
            .await
            .map_err(|e| anyhow::anyhow!("VpcStore scan allocations: {}", e))?;
        while let Some(kv) = iter
            .next()
            .await
            .map_err(|e| anyhow::anyhow!("VpcStore scan allocations next: {}", e))?
        {
            if !kv.key.starts_with(prefix) {
                break;
            }
            let alloc: StoredAllocation = serde_json::from_slice(&kv.value)?;
            allocs.push(alloc);
        }
        Ok(allocs)
    }

    pub async fn save_nft_snapshot(&self, snapshot: &str) -> anyhow::Result<()> {
        self.db
            .put(KEY_NFT_SNAPSHOT, snapshot.as_bytes().to_vec())
            .await
            .map_err(|e| anyhow::anyhow!("VpcStore save_nft_snapshot: {}", e))
    }

    pub async fn load_nft_snapshot(&self) -> anyhow::Result<Option<String>> {
        match self
            .db
            .get(KEY_NFT_SNAPSHOT)
            .await
            .map_err(|e| anyhow::anyhow!("VpcStore get nft_snapshot: {}", e))?
        {
            Some(b) => Ok(Some(String::from_utf8(b.to_vec())?)),
            None => Ok(None),
        }
    }

    pub async fn close(self) -> anyhow::Result<()> {
        self.db
            .close()
            .await
            .map_err(|e| anyhow::anyhow!("VpcStore close failed: {}", e))
    }
}
