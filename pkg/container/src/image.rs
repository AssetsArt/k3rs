use anyhow::Result;
use oci_client::{Client, Reference, client::ClientConfig, manifest::ImageIndexEntry};
use std::path::{Path, PathBuf};
use tracing::info;

/// Manages OCI image pulling and layer caching.
pub struct ImageManager {
    /// Root directory for image storage: `<data_dir>/images/`
    images_dir: PathBuf,
    /// OCI registry client
    client: Client,
}

impl ImageManager {
    pub fn new(data_dir: &Path) -> Self {
        let images_dir = data_dir.join("images");
        let config = ClientConfig {
            protocol: oci_client::client::ClientProtocol::HttpsExcept(vec![
                "localhost:5000".to_string(),
            ]),
            // Always resolve to linux/<host_arch> — container images are Linux-based,
            // even when running on macOS (VirtualizationBackend boots a Linux microVM).
            platform_resolver: Some(Box::new(linux_platform_resolver)),
            ..Default::default()
        };
        let client = Client::new(config);
        Self { images_dir, client }
    }

    /// Pull an image from a registry. Returns the path to the image directory.
    /// Layout: `<images_dir>/<image_hash>/` containing manifest.json + layer blobs.
    pub async fn pull(&self, image_ref: &str) -> Result<PathBuf> {
        let reference: Reference = image_ref
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid image reference '{}': {}", image_ref, e))?;

        info!("Pulling image: {}", reference);

        // Create image directory using a hash of the reference
        let image_hash = format!("{:x}", md5_hash(image_ref));
        let image_dir = self.images_dir.join(&image_hash);

        // Check if already pulled — manifest must exist AND at least one layer file.
        // An empty/partial cache (e.g. from a crashed download) is treated as a miss.
        let layers_dir = image_dir.join("layers");
        let has_layers = layers_dir.exists()
            && std::fs::read_dir(&layers_dir)
                .map(|mut rd| {
                    rd.any(|e| {
                        e.ok()
                            .and_then(|e| e.path().extension().map(|x| x == "gz"))
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false);

        if image_dir.join("manifest.json").exists() && has_layers {
            info!(
                "Image {} already cached at {}",
                image_ref,
                image_dir.display()
            );
            return Ok(image_dir);
        }

        tokio::fs::create_dir_all(&image_dir).await?;

        // Pull platform-resolved image manifest (auto-resolves multi-arch ImageIndex)
        let auth = oci_client::secrets::RegistryAuth::Anonymous;

        let (img_manifest, _digest) = self
            .client
            .pull_image_manifest(&reference, &auth)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to pull manifest for {}: {}", image_ref, e))?;

        // Save manifest
        let manifest_json = serde_json::to_string_pretty(&img_manifest)?;
        tokio::fs::write(image_dir.join("manifest.json"), &manifest_json).await?;

        // Pull each layer blob
        let layers_dir = image_dir.join("layers");
        tokio::fs::create_dir_all(&layers_dir).await?;

        for (i, layer) in img_manifest.layers.iter().enumerate() {
            let layer_path = layers_dir.join(format!("layer_{}.tar.gz", i));
            if layer_path.exists() {
                info!("  Layer {} already cached", i);
                continue;
            }

            info!(
                "  Pulling layer {}/{}: {} ({})",
                i + 1,
                img_manifest.layers.len(),
                layer.digest,
                format_size(layer.size as u64),
            );

            let mut layer_data = Vec::new();
            self.client
                .pull_blob(&reference, layer, &mut layer_data)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to pull layer {}: {}", layer.digest, e))?;

            tokio::fs::write(&layer_path, &layer_data).await?;
        }

        // Also pull the config blob
        let mut config_data = Vec::new();
        self.client
            .pull_blob(&reference, &img_manifest.config, &mut config_data)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to pull config: {}", e))?;
        tokio::fs::write(image_dir.join("config.json"), &config_data).await?;

        info!("Image {} pulled to {}", image_ref, image_dir.display());
        Ok(image_dir)
    }

    /// Get the cached image directory, if it exists.
    pub fn get_cached(&self, image_ref: &str) -> Option<PathBuf> {
        let image_hash = format!("{:x}", md5_hash(image_ref));
        let image_dir = self.images_dir.join(&image_hash);
        if image_dir.join("manifest.json").exists() {
            Some(image_dir)
        } else {
            None
        }
    }

    /// List all cached images with metadata.
    pub async fn list_images(&self) -> Result<Vec<ImageInfo>> {
        let mut images = Vec::new();

        if !self.images_dir.exists() {
            return Ok(images);
        }

        let mut entries = tokio::fs::read_dir(&self.images_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let manifest_path = path.join("manifest.json");
            if !manifest_path.exists() {
                continue;
            }

            // Read config.json for image metadata (cmd, env, etc.)
            let config_path = path.join("config.json");
            let (architecture, os, created) = if config_path.exists() {
                let data = tokio::fs::read_to_string(&config_path)
                    .await
                    .unwrap_or_default();
                let v: serde_json::Value = serde_json::from_str(&data).unwrap_or_default();
                (
                    v.get("architecture")
                        .and_then(|a| a.as_str())
                        .unwrap_or("unknown")
                        .to_string(),
                    v.get("os")
                        .and_then(|o| o.as_str())
                        .unwrap_or("unknown")
                        .to_string(),
                    v.get("created")
                        .and_then(|c| c.as_str())
                        .unwrap_or("")
                        .to_string(),
                )
            } else {
                ("unknown".to_string(), "unknown".to_string(), String::new())
            };

            // Calculate total size of all layers
            let layers_dir = path.join("layers");
            let mut total_size: u64 = 0;
            let mut layer_count: usize = 0;
            if layers_dir.exists() {
                let mut layer_entries = tokio::fs::read_dir(&layers_dir).await?;
                while let Some(le) = layer_entries.next_entry().await? {
                    if let Ok(meta) = le.metadata().await {
                        total_size += meta.len();
                        layer_count += 1;
                    }
                }
            }

            let hash = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();

            images.push(ImageInfo {
                id: hash,
                node_name: String::new(), // Set by server when aggregating across nodes
                size: total_size,
                size_human: format_size(total_size),
                layers: layer_count,
                architecture,
                os,
                created,
            });
        }

        images.sort_by_key(|i| std::cmp::Reverse(i.size));
        Ok(images)
    }

    /// Delete a cached image by its hash ID.
    pub async fn delete_image(&self, image_id: &str) -> Result<()> {
        let image_dir = self.images_dir.join(image_id);
        if image_dir.exists() {
            tokio::fs::remove_dir_all(&image_dir).await?;
            info!("Deleted cached image: {}", image_id);
        } else {
            anyhow::bail!("Image {} not found", image_id);
        }
        Ok(())
    }
}

/// Metadata about a cached OCI image.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ImageInfo {
    pub id: String,
    pub node_name: String,
    pub size: u64,
    pub size_human: String,
    pub layers: usize,
    pub architecture: String,
    pub os: String,
    pub created: String,
}

/// Simple hash for image reference → directory name.
fn md5_hash(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

fn format_size(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1_000 {
        format!("{:.0} KB", bytes as f64 / 1_024.0)
    } else {
        format!("{} B", bytes)
    }
}

/// Platform resolver that picks the first `linux/<host_arch>` entry from an
/// OCI Image Index. This ensures images resolve correctly on macOS (where the
/// host OS is `darwin`) since all container images target Linux.
fn linux_platform_resolver(manifests: &[ImageIndexEntry]) -> Option<String> {
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "amd64",
        other => other,
    };

    // First try: exact match linux/<host_arch>
    for entry in manifests {
        if let Some(platform) = &entry.platform
            && platform.os == "linux"
            && platform.architecture == arch
        {
            return Some(entry.digest.clone());
        }
    }

    // Fallback: any linux image
    for entry in manifests {
        if let Some(platform) = &entry.platform
            && platform.os == "linux"
        {
            return Some(entry.digest.clone());
        }
    }

    None
}
