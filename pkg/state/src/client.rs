use slatedb::Db;
use slatedb::object_store::local::LocalFileSystem;
use slatedb::object_store::path::Path;
use std::sync::Arc;
use tracing::info;

/// Persistent state store backed by SlateDB on a local filesystem.
/// In production this would use S3/R2/MinIO via the `object_store` crate.
#[derive(Clone)]
pub struct StateStore {
    db: Db,
}

impl StateStore {
    /// Open (or create) a state store rooted at `path` on the local filesystem.
    pub async fn new(path: &str) -> anyhow::Result<Self> {
        info!("Opening SlateDB state store at {}", path);

        // Ensure the data directory exists before opening the object store
        std::fs::create_dir_all(path)
            .map_err(|e| anyhow::anyhow!("Failed to create data directory {}: {}", path, e))?;

        let object_store = Arc::new(
            LocalFileSystem::new_with_prefix(path)
                .map_err(|e| anyhow::anyhow!("Failed to create local object store: {}", e))?,
        );
        let db = Db::open(Path::from("/"), object_store)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to open SlateDB: {}", e))?;
        Ok(Self { db })
    }

    /// Store a value under the given key.
    pub async fn put(&self, key: &str, value: &[u8]) -> anyhow::Result<()> {
        self.db
            .put(key.as_bytes(), value)
            .await
            .map_err(|e| anyhow::anyhow!("SlateDB put failed: {}", e))
    }

    /// Retrieve the value for a key, or `None` if it does not exist.
    pub async fn get(&self, key: &str) -> anyhow::Result<Option<Vec<u8>>> {
        match self.db.get(key.as_bytes()).await {
            Ok(Some(bytes)) => Ok(Some(bytes.to_vec())),
            Ok(None) => Ok(None),
            Err(e) => Err(anyhow::anyhow!("SlateDB get failed: {}", e)),
        }
    }

    /// Delete a key from the store.
    pub async fn delete(&self, key: &str) -> anyhow::Result<()> {
        self.db
            .delete(key.as_bytes())
            .await
            .map_err(|e| anyhow::anyhow!("SlateDB delete failed: {}", e))
    }

    /// List all key-value pairs whose keys start with `prefix`.
    /// Returns them as `(key_string, raw_bytes)`.
    pub async fn list_prefix(&self, prefix: &str) -> anyhow::Result<Vec<(String, Vec<u8>)>> {
        let mut results = Vec::new();
        let mut iter = self
            .db
            .scan_prefix(prefix.as_bytes())
            .await
            .map_err(|e| anyhow::anyhow!("SlateDB scan_prefix failed: {}", e))?;

        while let Ok(Some(kv)) = iter.next().await {
            let key = String::from_utf8_lossy(&kv.key).to_string();
            results.push((key, kv.value.to_vec()));
        }
        Ok(results)
    }

    /// Gracefully close the state store.
    pub async fn close(self) -> anyhow::Result<()> {
        info!("Closing SlateDB state store");
        self.db
            .close()
            .await
            .map_err(|e| anyhow::anyhow!("SlateDB close failed: {}", e))
    }
}
