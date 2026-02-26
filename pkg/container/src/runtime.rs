use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;

use crate::backend::{OciBackend, RuntimeBackend, StubBackend};
use crate::image::ImageManager;
use crate::rootfs::RootfsManager;

/// Runtime info for tracking which backend a pod is using.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RuntimeInfo {
    pub backend: String,
    pub version: String,
    #[serde(default)]
    pub path: String,
}

/// Container runtime — orchestrates image pull, rootfs extraction, and
/// container lifecycle via a pluggable RuntimeBackend.
pub struct ContainerRuntime {
    backend: Arc<dyn RuntimeBackend>,
    image_manager: ImageManager,
    data_dir: PathBuf,
}

impl ContainerRuntime {
    /// Create a new container runtime with automatic backend detection.
    ///
    /// Detection priority:
    /// - macOS: Docker → Stub
    /// - Linux: OCI (youki/crun/runc in PATH) → auto-download → Stub
    pub async fn new(data_dir: Option<&str>) -> Result<Self> {
        let data_dir = PathBuf::from(data_dir.unwrap_or("/tmp/k3rs-runtime"));
        tokio::fs::create_dir_all(&data_dir).await?;
        tokio::fs::create_dir_all(data_dir.join("containers")).await?;

        let backend: Arc<dyn RuntimeBackend> = if cfg!(target_os = "macos") {
            // macOS: try Docker first
            match crate::backend::DockerBackend::detect() {
                Ok(docker) => {
                    info!(
                        "Using Docker runtime: {} ({})",
                        docker.name(),
                        docker.version()
                    );
                    Arc::new(docker)
                }
                Err(e) => {
                    info!("Docker not available ({}), falling back to stub mode", e);
                    Arc::new(StubBackend)
                }
            }
        } else {
            // Linux: try OCI runtimes
            match OciBackend::detect() {
                Ok(oci) => {
                    info!("Using OCI runtime: {} ({})", oci.name(), oci.version());
                    Arc::new(oci)
                }
                Err(_) => {
                    // Try auto-download
                    info!("No OCI runtime in PATH — attempting auto-download...");
                    match crate::installer::RuntimeInstaller::ensure_runtime(None).await {
                        Ok(path) => {
                            let oci = OciBackend::new(&path.to_string_lossy());
                            info!(
                                "Using auto-downloaded runtime: {} ({})",
                                oci.name(),
                                oci.version()
                            );
                            Arc::new(oci)
                        }
                        Err(e) => {
                            info!("Auto-download failed ({}), falling back to stub mode", e);
                            Arc::new(StubBackend)
                        }
                    }
                }
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

    /// Get full runtime info for pod tracking.
    pub fn runtime_info(&self) -> RuntimeInfo {
        RuntimeInfo {
            backend: self.backend.name().to_string(),
            version: self.backend.version().to_string(),
            path: String::new(),
        }
    }

    // ─── Image Operations ───────────────────────────────────────────

    pub async fn pull_image(&self, image: &str) -> Result<()> {
        if self.backend.handles_images() {
            // Docker handles images internally — skip OCI pull
            info!(
                "Skipping OCI image pull (handled by {} backend)",
                self.backend.name()
            );
            return Ok(());
        }
        self.image_manager.pull(image).await?;
        Ok(())
    }

    // ─── Container Lifecycle ────────────────────────────────────────

    pub async fn create_container(&self, id: &str, image: &str, command: &[String]) -> Result<()> {
        info!(
            "Creating container: id={}, image={}, backend={}",
            id,
            image,
            self.backend.name()
        );

        if self.backend.handles_images() {
            // Docker backend: create directly from image
            self.backend.create_from_image(id, image, command).await?;
        } else {
            // OCI backend: pull image → extract rootfs → create bundle
            let image_dir = self.image_manager.pull(image).await?;

            let container_dir = self.data_dir.join("containers").join(id);
            tokio::fs::create_dir_all(&container_dir).await?;

            let rootfs_path = RootfsManager::extract(&image_dir, &container_dir).await?;
            let config_json = RootfsManager::generate_config(id, &rootfs_path, command)?;
            tokio::fs::write(container_dir.join("config.json"), &config_json).await?;

            self.backend.create(id, &container_dir).await?;
        }

        info!(
            "Container {} created successfully via {}",
            id,
            self.backend.name()
        );
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

    /// Execute a command inside a running container.
    pub async fn exec_in_container(&self, id: &str, command: &[&str]) -> Result<String> {
        self.backend.exec(id, command).await
    }

    // ─── Image Management ───────────────────────────────────────

    pub async fn list_images(&self) -> Result<Vec<crate::image::ImageInfo>> {
        self.image_manager.list_images().await
    }

    pub async fn delete_image(&self, image_id: &str) -> Result<()> {
        self.image_manager.delete_image(image_id).await
    }
}
