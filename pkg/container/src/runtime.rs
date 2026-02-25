use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;

use crate::backend::{OciBackend, RuntimeBackend, StubBackend};
use crate::image::ImageManager;
use crate::rootfs::RootfsManager;

/// Container runtime — orchestrates image pull, rootfs extraction, and
/// container lifecycle via a pluggable RuntimeBackend.
pub struct ContainerRuntime {
    backend: Arc<dyn RuntimeBackend>,
    image_manager: ImageManager,
    data_dir: PathBuf,
}

impl ContainerRuntime {
    /// Create a new container runtime with automatic OCI runtime detection.
    /// Falls back to stub mode if no runtime (youki/crun/runc) is found.
    pub async fn new(data_dir: Option<&str>) -> Result<Self> {
        let data_dir = PathBuf::from(data_dir.unwrap_or("/var/run/k3rs"));
        tokio::fs::create_dir_all(&data_dir).await?;
        tokio::fs::create_dir_all(data_dir.join("containers")).await?;

        let backend: Arc<dyn RuntimeBackend> = match OciBackend::detect() {
            Ok(oci) => {
                info!("Using OCI runtime: {}", oci.name());
                Arc::new(oci)
            }
            Err(_) => {
                info!("No OCI runtime found — falling back to stub mode");
                Arc::new(StubBackend)
            }
        };

        let image_manager = ImageManager::new(&data_dir);

        Ok(Self {
            backend,
            image_manager,
            data_dir,
        })
    }

    /// Create a stub-mode runtime (for backwards compatibility).
    pub fn new_stub() -> Self {
        let data_dir = PathBuf::from("/tmp/k3rs-stub");
        let _ = std::fs::create_dir_all(&data_dir);
        let _ = std::fs::create_dir_all(data_dir.join("containers"));
        Self {
            backend: Arc::new(StubBackend),
            image_manager: ImageManager::new(&data_dir),
            data_dir,
        }
    }

    /// The name of the active runtime backend.
    pub fn backend_name(&self) -> &str {
        self.backend.name()
    }

    // ─── Image Operations ───────────────────────────────────────────

    pub async fn pull_image(&self, image: &str) -> Result<()> {
        self.image_manager.pull(image).await?;
        Ok(())
    }

    // ─── Container Lifecycle ────────────────────────────────────────

    pub async fn create_container(&self, id: &str, image: &str, command: &[String]) -> Result<()> {
        info!("Creating container: id={}, image={}", id, image);

        // 1. Ensure image is pulled
        let image_dir = self.image_manager.pull(image).await?;

        // 2. Create container directory
        let container_dir = self.data_dir.join("containers").join(id);
        tokio::fs::create_dir_all(&container_dir).await?;

        // 3. Extract rootfs from image layers
        let rootfs_path = RootfsManager::extract(&image_dir, &container_dir).await?;

        // 4. Generate OCI config.json
        let config_json = RootfsManager::generate_config(id, &rootfs_path, command)?;
        tokio::fs::write(container_dir.join("config.json"), &config_json).await?;

        // 5. Create the container via runtime backend
        self.backend.create(id, &container_dir).await?;

        info!("Container {} created successfully", id);
        Ok(())
    }

    pub async fn start_container(&self, id: &str) -> Result<()> {
        self.backend.start(id).await
    }

    pub async fn stop_container(&self, id: &str) -> Result<()> {
        self.backend.stop(id).await?;
        self.backend.delete(id).await?;
        Ok(())
    }

    pub async fn list_containers(&self) -> Result<Vec<String>> {
        self.backend.list().await
    }

    pub async fn container_logs(&self, id: &str, tail: usize) -> Result<Vec<String>> {
        self.backend.logs(id, tail).await
    }

    // ─── Image Management ───────────────────────────────────────

    pub async fn list_images(&self) -> Result<Vec<crate::image::ImageInfo>> {
        self.image_manager.list_images().await
    }

    pub async fn delete_image(&self, image_id: &str) -> Result<()> {
        self.image_manager.delete_image(image_id).await
    }
}
