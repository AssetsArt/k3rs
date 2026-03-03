use chrono::Utc;
use flate2::Compression;
use flate2::write::GzEncoder;
use pkg_state::client::StateStore;
use pkg_state::watch::EventType;
use pkg_types::backup::{BACKUP_VERSION, BackupEntry, BackupFile, BackupPki, BackupStatus};
use std::io::Write;
use std::time::Duration;
use tracing::{error, info, warn};

/// Controller that periodically snapshots cluster state to disk.
///
/// Runs only on the leader. Writes `backup-YYYYMMDD-HHmmss.k3rs-backup.json.gz`
/// to `backup_dir` and rotates old files to keep at most `retention` copies.
pub struct BackupController {
    store: StateStore,
    backup_dir: String,
    interval: Duration,
    retention: usize,
    ca_cert_pem: String,
}

impl BackupController {
    pub fn new(
        store: StateStore,
        backup_dir: impl Into<String>,
        interval_secs: u64,
        retention: usize,
        ca_cert_pem: impl Into<String>,
    ) -> Self {
        Self {
            store,
            backup_dir: backup_dir.into(),
            interval: Duration::from_secs(interval_secs),
            retention,
            ca_cert_pem: ca_cert_pem.into(),
        }
    }

    pub fn start(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            info!(
                "BackupController started (interval={}s, dir={}, retention={})",
                self.interval.as_secs(),
                self.backup_dir,
                self.retention
            );

            if let Err(e) = std::fs::create_dir_all(&self.backup_dir) {
                error!(
                    "BackupController: cannot create backup_dir {}: {}",
                    self.backup_dir, e
                );
                return;
            }

            let mut interval = tokio::time::interval(self.interval);
            // Skip the very first immediate tick so we don't backup at startup.
            interval.tick().await;

            loop {
                interval.tick().await;
                match self.run_backup().await {
                    Ok(filename) => {
                        info!("BackupController: backup written to {}", filename);
                        // Emit success event
                        self.store
                            .event_log
                            .emit(
                                EventType::Put,
                                "/events/backup/success".to_string(),
                                Some(filename.into_bytes()),
                            )
                            .await;
                    }
                    Err(e) => {
                        warn!("BackupController: backup failed: {}", e);
                        // Emit failure event
                        self.store
                            .event_log
                            .emit(
                                EventType::Put,
                                "/events/backup/failure".to_string(),
                                Some(e.to_string().into_bytes()),
                            )
                            .await;
                    }
                }
            }
        })
    }

    /// Run one backup cycle: snapshot → gzip → write → rotate → update metadata.
    async fn run_backup(&self) -> anyhow::Result<String> {
        let entries_raw = self.store.snapshot().await?;

        let node_count = entries_raw
            .iter()
            .filter(|(k, _)| k.starts_with("/registry/nodes/"))
            .count();

        let entries: Vec<BackupEntry> = entries_raw
            .into_iter()
            .filter_map(|(key, value)| {
                let v = serde_json::from_slice(&value).ok()?;
                Some(BackupEntry { key, value: v })
            })
            .collect();

        let key_count = entries.len();
        let now = Utc::now();

        let backup = BackupFile {
            version: BACKUP_VERSION.to_string(),
            created_at: now,
            cluster_name: "k3rs".to_string(),
            node_count,
            key_count,
            entries,
            pki: BackupPki {
                ca_cert: self.ca_cert_pem.clone(),
            },
        };

        // Gzip-compress
        let json = serde_json::to_vec_pretty(&backup)?;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&json)?;
        let compressed = encoder.finish()?;

        // Write to disk
        let filename = format!("backup-{}.k3rs-backup.json.gz", now.format("%Y%m%d-%H%M%S"));
        let path = format!("{}/{}", self.backup_dir, filename);
        tokio::fs::write(&path, &compressed).await?;
        info!(
            "BackupController: wrote {} ({} entries, {} bytes)",
            filename,
            key_count,
            compressed.len()
        );

        // Rotate old backups
        self.rotate_backups().await?;

        // Persist last-backup metadata to store
        let status = BackupStatus {
            last_backup_at: Some(now),
            last_backup_file: Some(path.clone()),
            key_count: Some(key_count),
            status: "success".to_string(),
        };
        if let Ok(data) = serde_json::to_vec(&status) {
            self.store.put("/registry/_backup/last", &data).await?;
        }

        Ok(path)
    }

    /// Remove backup files beyond the retention limit (keeps newest `retention` files).
    async fn rotate_backups(&self) -> anyhow::Result<()> {
        let mut dir = tokio::fs::read_dir(&self.backup_dir).await?;
        let mut files: Vec<(String, std::time::SystemTime)> = Vec::new();

        while let Ok(Some(entry)) = dir.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".k3rs-backup.json.gz") {
                if let Ok(meta) = entry.metadata().await {
                    if let Ok(mtime) = meta.modified() {
                        files.push((entry.path().to_string_lossy().to_string(), mtime));
                    }
                }
            }
        }

        // Sort newest first
        files.sort_by(|a, b| b.1.cmp(&a.1));

        // Delete files beyond retention
        for (path, _) in files.into_iter().skip(self.retention) {
            match tokio::fs::remove_file(&path).await {
                Ok(()) => info!("BackupController: rotated out old backup {}", path),
                Err(e) => warn!("BackupController: failed to remove {}: {}", path, e),
            }
        }

        Ok(())
    }
}
