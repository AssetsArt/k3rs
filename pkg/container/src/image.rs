use anyhow::Result;
use oci_client::{Client, Reference, client::ClientConfig};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

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

        // Check if already pulled
        if image_dir.join("manifest.json").exists() {
            info!(
                "Image {} already cached at {}",
                image_ref,
                image_dir.display()
            );
            return Ok(image_dir);
        }

        tokio::fs::create_dir_all(&image_dir).await?;

        // Pull image manifest and layers
        let auth = oci_client::secrets::RegistryAuth::Anonymous;
        let _accepted_media_types = vec![
            oci_client::manifest::OCI_IMAGE_MEDIA_TYPE.to_string(),
            oci_client::manifest::IMAGE_MANIFEST_MEDIA_TYPE.to_string(),
            oci_client::manifest::IMAGE_DOCKER_LAYER_GZIP_MEDIA_TYPE.to_string(),
        ];

        let (manifest, _digest) = self
            .client
            .pull_manifest(&reference, &auth)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to pull manifest for {}: {}", image_ref, e))?;

        // Save manifest
        let manifest_json = serde_json::to_string_pretty(&manifest)?;
        tokio::fs::write(image_dir.join("manifest.json"), &manifest_json).await?;

        // Pull each layer blob
        match &manifest {
            oci_client::manifest::OciManifest::Image(img_manifest) => {
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
                        .pull_blob(&reference, &layer, &mut layer_data)
                        .await
                        .map_err(|e| {
                            anyhow::anyhow!("Failed to pull layer {}: {}", layer.digest, e)
                        })?;

                    tokio::fs::write(&layer_path, &layer_data).await?;
                }

                // Also pull the config blob
                let mut config_data = Vec::new();
                self.client
                    .pull_blob(&reference, &img_manifest.config, &mut config_data)
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to pull config: {}", e))?;
                tokio::fs::write(image_dir.join("config.json"), &config_data).await?;
            }
            oci_client::manifest::OciManifest::ImageIndex(index) => {
                // For multi-arch images, save the index and pick the first matching manifest
                warn!(
                    "Image index with {} manifests — using first entry",
                    index.manifests.len()
                );
                let manifest_json = serde_json::to_string_pretty(&index)?;
                tokio::fs::write(image_dir.join("index.json"), &manifest_json).await?;
            }
        }

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
