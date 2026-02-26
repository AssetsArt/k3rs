use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;

use crate::backend::{OciBackend, RuntimeBackend};
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
    /// - macOS: VirtualizationBackend (Apple Virtualization.framework microVM)
    /// - Linux: OCI (youki/crun in PATH) → auto-download
    pub async fn new(data_dir: Option<&str>) -> Result<Self> {
        let data_dir = PathBuf::from(data_dir.unwrap_or("/tmp/k3rs-runtime"));
        tokio::fs::create_dir_all(&data_dir).await.map_err(|e| {
            tracing::error!("[runtime] create_dir_all error: {}", e);
            e
        })?;
        tokio::fs::create_dir_all(data_dir.join("containers"))
            .await
            .map_err(|e| {
                tracing::error!("[runtime] create_dir_all error: {}", e);
                e
            })?;

        let backend: Arc<dyn RuntimeBackend> = if cfg!(target_os = "macos") {
            // macOS: use Virtualization.framework microVM backend
            match crate::virt::VirtualizationBackend::new(&data_dir).await {
                Ok(virt) => {
                    info!(
                        "Using Virtualization.framework runtime: {} ({})",
                        virt.name(),
                        virt.version()
                    );
                    Arc::new(virt)
                }
                Err(e) => {
                    info!(
                        "Virtualization.framework not available ({}), trying OCI fallback",
                        e
                    );
                    // Fallback to OCI if available
                    match OciBackend::detect() {
                        Ok(oci) => {
                            info!("Using OCI runtime: {} ({})", oci.name(), oci.version());
                            Arc::new(oci)
                        }
                        Err(e2) => {
                            anyhow::bail!(
                                "No container runtime available. \
                                 Virtualization.framework: {}. OCI: {}",
                                e,
                                e2
                            );
                        }
                    }
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
                            anyhow::bail!(
                                "No container runtime available. \
                                 OCI auto-download failed: {}",
                                e
                            );
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
            // Backend handles images internally
            self.backend.create_from_image(id, image, command).await?;
        } else {
            // Pull image → extract rootfs → create bundle → create via backend
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
