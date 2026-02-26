use anyhow::Result;
use std::path::{Path, PathBuf};
use tracing::info;

/// Current user's UID for rootless container user namespace mappings.
fn host_uid() -> u32 {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
        .unwrap_or(1000)
}

/// Current user's GID for rootless container user namespace mappings.
fn host_gid() -> u32 {
    std::process::Command::new("id")
        .arg("-g")
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
        .unwrap_or(1000)
}

/// Manages rootfs extraction from OCI image layers.
pub struct RootfsManager;

impl RootfsManager {
    /// Extract OCI image layers into a rootfs directory.
    /// Applies layers in order (lower → upper) to form the final filesystem.
    /// Returns the rootfs path.
    pub async fn extract(image_dir: &Path, container_dir: &Path) -> Result<PathBuf> {
        let rootfs = container_dir.join("rootfs");
        tokio::fs::create_dir_all(&rootfs).await?;

        let layers_dir = image_dir.join("layers");
        if !layers_dir.exists() {
            anyhow::bail!("No layers directory found at {}", layers_dir.display());
        }

        // Collect and sort layer files
        let mut layers: Vec<PathBuf> = Vec::new();
        let mut entries = tokio::fs::read_dir(&layers_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("gz") {
                layers.push(path);
            }
        }
        layers.sort();

        info!("Extracting {} layers to {}", layers.len(), rootfs.display());

        // Extract each layer — must be done on a blocking thread (tar is sync)
        for (i, layer_path) in layers.iter().enumerate() {
            info!(
                "  Extracting layer {}/{}: {}",
                i + 1,
                layers.len(),
                layer_path.display()
            );

            let layer_path = layer_path.clone();
            let rootfs_clone = rootfs.clone();

            tokio::task::spawn_blocking(move || -> Result<()> {
                let file = std::fs::File::open(&layer_path)?;
                let decoder = flate2::read::GzDecoder::new(file);
                let mut archive = tar::Archive::new(decoder);

                // Unpack into rootfs, skip whiteout processing for simplicity
                archive.set_preserve_permissions(true);
                archive.set_overwrite(true);
                archive.unpack(&rootfs_clone)?;
                Ok(())
            })
            .await??;
        }

        info!("Rootfs extracted to {}", rootfs.display());
        Ok(rootfs)
    }

    /// Generate an OCI runtime config.json for the container.
    pub fn generate_config(
        container_id: &str,
        _rootfs_path: &Path,
        command: &[String],
    ) -> Result<String> {
        let cmd = if command.is_empty() {
            vec!["/bin/sh"]
        } else {
            command.iter().map(|s| s.as_str()).collect()
        };

        let config = serde_json::json!({
            "ociVersion": "1.0.2",
            "process": {
                "terminal": false,
                "user": { "uid": 0, "gid": 0 },
                "args": cmd,
                "env": [
                    "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                    format!("HOSTNAME={}", container_id),
                ],
                "cwd": "/"
            },
            "root": {
                "path": "rootfs",
                "readonly": false
            },
            "hostname": container_id,
            "annotations": {
                "run.oci.keep_original_groups": "1"
            },
            "mounts": [
                { "destination": "/proc", "type": "proc", "source": "proc" },
                { "destination": "/dev",  "type": "tmpfs", "source": "tmpfs", "options": ["nosuid", "strictatime", "mode=755", "size=65536k"] },
                { "destination": "/sys",  "type": "sysfs", "source": "sysfs", "options": ["nosuid", "noexec", "nodev", "ro"] },
            ],
            "linux": {
                "namespaces": [
                    { "type": "pid" },
                    { "type": "ipc" },
                    { "type": "uts" },
                    { "type": "mount" },
                    { "type": "user" },
                ],
                "uidMappings": [
                    { "containerID": 0, "hostID": host_uid(), "size": 65536 }
                ],
                "gidMappings": [
                    { "containerID": 0, "hostID": host_gid(), "size": 65536 }
                ]
            }
        });

        let config_json = serde_json::to_string_pretty(&config)?;
        Ok(config_json)
    }
}
