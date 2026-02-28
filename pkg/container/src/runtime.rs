use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{error, info};

use crate::backend::{OciBackend, RuntimeBackend};
use crate::image::ImageManager;
use crate::rootfs::RootfsManager;
use crate::state::{ContainerState, ContainerStateInfo, ContainerStore};

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
///
/// No mocking — every operation calls real OCI runtime binaries.
/// Container state is tracked in-process via `ContainerStore`.
pub struct ContainerRuntime {
    backend: Arc<dyn RuntimeBackend>,
    image_manager: ImageManager,
    data_dir: PathBuf,
    /// In-process container state tracker.
    store: ContainerStore,
}

impl ContainerRuntime {
    /// Create a new container runtime with automatic backend detection.
    ///
    /// Detection priority:
    /// - macOS: VirtualizationBackend (Apple Virtualization.framework microVM)
    /// - Linux: OCI (youki/crun in PATH) → auto-download
    pub async fn new(data_dir: Option<&str>) -> Result<Self> {
        let data_dir =
            PathBuf::from(data_dir.unwrap_or(pkg_constants::paths::DEFAULT_RUNTIME_DATA_DIR));
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
                    match OciBackend::detect(&data_dir) {
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
            // Linux: prefer OCI runtimes; Firecracker is only a fallback when
            // no OCI runtime is available but KVM is present.
            // This ensures backend_name() correctly reflects the active runtime
            // (e.g. "youki") rather than always showing "vm".
            match OciBackend::detect(&data_dir) {
                Ok(oci) => {
                    info!("Using OCI runtime: {} ({})", oci.name(), oci.version());
                    Arc::new(oci)
                }
                Err(_) => {
                    // No OCI runtime in PATH or install dirs — try auto-download.
                    info!("No OCI runtime found — attempting auto-download...");
                    match crate::installer::RuntimeInstaller::ensure_runtime(None).await {
                        Ok(path) => {
                            let oci = OciBackend::new(&path.to_string_lossy(), &data_dir);
                            info!(
                                "Using auto-downloaded runtime: {} ({})",
                                oci.name(),
                                oci.version()
                            );
                            Arc::new(oci)
                        }
                        Err(_) => {
                            // Last resort: Firecracker if KVM is available.
                            let has_kvm = std::path::Path::new("/dev/kvm").exists();
                            if has_kvm {
                                info!(
                                    "OCI unavailable — falling back to Firecracker microVM runtime"
                                );
                                Arc::new(crate::firecracker::FirecrackerBackend::new(&data_dir))
                            } else {
                                anyhow::bail!(
                                    "No container runtime available: OCI auto-download failed \
                                     and /dev/kvm not present for Firecracker fallback"
                                );
                            }
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
            store: ContainerStore::new(),
        })
    }

    /// The name of the active runtime backend.
    pub fn backend_name(&self) -> &str {
        self.backend.name()
    }

    /// Returns the path to the OCI runtime binary (e.g. `/usr/local/bin/youki`),
    /// or None for VM backends. Used by the agent for PTY-based exec.
    pub fn oci_runtime_path(&self) -> Option<String> {
        self.backend.oci_runtime_path()
    }

    /// Get full runtime info for pod tracking.
    pub fn runtime_info(&self) -> RuntimeInfo {
        RuntimeInfo {
            backend: self.backend.name().to_string(),
            version: self.backend.version().to_string(),
            path: String::new(),
        }
    }

    /// Access the container state store.
    pub fn container_store(&self) -> &ContainerStore {
        &self.store
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

    /// Create a container from an OCI image.
    ///
    /// Full pipeline: pull image → extract rootfs → generate config.json → create via backend.
    /// Accepts optional environment variables from the pod's `ContainerSpec`.
    pub async fn create_container(
        &self,
        id: &str,
        image: &str,
        command: &[String],
        env: &HashMap<String, String>,
        runtime_name: Option<&str>,
    ) -> Result<()> {
        let backend = if let Some(name) = runtime_name {
            if name == "vm" {
                if cfg!(target_os = "macos") {
                    if self.backend.name() == "vm" {
                        self.backend.clone()
                    } else {
                        Arc::new(crate::virt::VirtualizationBackend::new(&self.data_dir).await?)
                            as Arc<dyn RuntimeBackend>
                    }
                } else if cfg!(target_os = "linux") {
                    if self.backend.name() == "vm" {
                        self.backend.clone()
                    } else {
                        Arc::new(crate::firecracker::FirecrackerBackend::new(&self.data_dir))
                            as Arc<dyn RuntimeBackend>
                    }
                } else {
                    info!("Requested vm runtime not supported on this platform, using default");
                    self.backend.clone()
                }
            } else if name == "youki" || name == "crun" {
                // Try PATH first; auto-download if not found.
                match OciBackend::with_name(name, &self.data_dir) {
                    Ok(b) => Arc::new(b) as Arc<dyn RuntimeBackend>,
                    Err(_) => {
                        info!(
                            "OCI runtime {} not found in PATH or install dir — attempting auto-download...",
                            name
                        );
                        let downloaded = crate::installer::RuntimeInstaller::ensure_runtime(None)
                            .await
                            .map_err(|e| {
                                anyhow::anyhow!("Auto-download failed for {}: {}", name, e)
                            })?;
                        Arc::new(OciBackend::new(
                            &downloaded.to_string_lossy(),
                            &self.data_dir,
                        )) as Arc<dyn RuntimeBackend>
                    }
                }
            } else {
                info!("Requested runtime {} not available, using default", name);
                self.backend.clone()
            }
        } else {
            self.backend.clone()
        };

        info!(
            "Creating container: id={}, image={}, backend={}",
            id,
            image,
            backend.name()
        );

        let container_dir = self.data_dir.join("containers").join(id);
        let log_path = self.data_dir.join("logs").join(id).join("stdout.log");

        // Best-effort cleanup in case old state exists (e.g. from a previous failed run)
        // to avoid "container already exists" errors.
        if let Ok(state) = backend.state(id).await {
            if state.status == "stopped" || state.status == "exited" {
                info!(
                    "Container {} exists in stopped/exited state, cleaning up first...",
                    id
                );
                let _ = backend.delete(id).await;
            }
        }

        if backend.handles_images() {
            // Backend handles images internally (e.g. Docker)
            backend.create_from_image(id, image, command).await?;
        } else {
            // Pull image → extract rootfs → create bundle → create via backend
            let image_dir = self.image_manager.pull(image).await?;

            tokio::fs::create_dir_all(&container_dir).await?;

            let rootfs_path = RootfsManager::extract(&image_dir, &container_dir).await?;
            let config_json = RootfsManager::generate_config_full(
                id,
                &rootfs_path,
                command,
                env,
                Some(&image_dir),
                None,
                crate::rootfs::NetworkMode::default(),
            )?;
            tokio::fs::write(container_dir.join("config.json"), &config_json).await?;

            backend.create(id, &container_dir).await?;
        }

        self.store.track(
            id,
            image,
            backend.name(),
            &container_dir.to_string_lossy(),
            &log_path.to_string_lossy(),
        );

        info!(
            "Container {} created successfully via {}",
            id,
            backend.name()
        );
        Ok(())
    }

    /// Helper to get the correct backend for a specific container.
    async fn get_backend_for_container(&self, id: &str) -> Arc<dyn RuntimeBackend> {
        if let Some(entry) = self.store.get(id) {
            if entry.runtime_name == "vm" {
                if cfg!(target_os = "macos") {
                    if self.backend.name() == "vm" {
                        return self.backend.clone();
                    } else {
                        // This shouldn't happen often, but handle fallback
                        if let Ok(virt) =
                            crate::virt::VirtualizationBackend::new(&self.data_dir).await
                        {
                            return Arc::new(virt);
                        }
                    }
                } else if cfg!(target_os = "linux") {
                    if self.backend.name() == "vm" {
                        return self.backend.clone();
                    } else {
                        return Arc::new(crate::firecracker::FirecrackerBackend::new(
                            &self.data_dir,
                        ));
                    }
                }
            } else if entry.runtime_name == "youki" || entry.runtime_name == "crun" {
                if let Ok(oci) = OciBackend::with_name(&entry.runtime_name, &self.data_dir) {
                    return Arc::new(oci);
                }
            }
        }
        // Fallback to default backend
        self.backend.clone()
    }

    /// Start a created container.
    pub async fn start_container(&self, id: &str) -> Result<()> {
        let backend = self.get_backend_for_container(id).await;
        backend.start(id).await?;
        self.store.update_state(id, ContainerState::Running);
        info!("Container {} started via {}", id, backend.name());
        Ok(())
    }

    /// Stop and delete a container.
    pub async fn stop_container(&self, id: &str) -> Result<()> {
        let backend = self.get_backend_for_container(id).await;
        backend.stop(id).await?;
        backend.delete(id).await?;
        self.store.update_state(id, ContainerState::Stopped);
        info!(
            "Container {} stopped and deleted via {}",
            id,
            backend.name()
        );
        Ok(())
    }

    /// Mark a container as failed (e.g. due to a startup error).
    pub fn mark_failed(&self, id: &str, reason: &str) {
        self.store
            .update_state(id, ContainerState::Failed(reason.to_string()));
        error!("Container {} failed: {}", id, reason);
    }

    /// List running containers from the backend.
    pub async fn list_containers(&self) -> Result<Vec<String>> {
        self.backend.list().await
    }

    /// Discover running containers from the backend and populate the state store.
    pub async fn discover_running_containers(&self) -> Result<Vec<String>> {
        info!(
            "Discovering running containers from {} backend...",
            self.backend.name()
        );
        let mut discovered = Vec::new();

        let ids = self.backend.list().await.unwrap_or_default();
        for id in ids {
            match self.backend.state(&id).await {
                Ok(state_info) => {
                    if state_info.status == "running" || state_info.status == "created" {
                        info!(
                            "Discovered container: {} (status: {})",
                            id, state_info.status
                        );

                        let bundle_path = state_info.bundle.clone();
                        let log_path = self.data_dir.join("logs").join(&id).join("stdout.log");

                        self.store.track(
                            &id,
                            "recovered",
                            self.backend.name(),
                            &bundle_path,
                            &log_path.to_string_lossy(),
                        );

                        if state_info.status == "running" {
                            self.store.update_state(&id, ContainerState::Running);
                            if state_info.pid > 0 {
                                self.store.set_pid(&id, state_info.pid);
                            }
                        }
                        discovered.push(id.clone());
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to get state for discovered container {}: {}", id, e);
                }
            }
        }

        info!("Discovered {} running/created containers", discovered.len());
        Ok(discovered)
    }

    /// Get logs from a container.
    pub async fn container_logs(&self, id: &str, tail: usize) -> Result<Vec<String>> {
        let backend = self.get_backend_for_container(id).await;
        backend.logs(id, tail).await
    }

    /// Execute a command inside a running container.
    pub async fn exec_in_container(&self, id: &str, command: &[&str]) -> Result<String> {
        let backend = self.get_backend_for_container(id).await;
        backend.exec(id, command).await
    }

    /// Spawn an interactive command inside a running container.
    /// `tty` — allocate a pseudo-terminal inside the container.
    pub async fn spawn_exec_in_container(
        &self,
        id: &str,
        command: &[&str],
        tty: bool,
    ) -> Result<tokio::process::Child> {
        let backend = self.get_backend_for_container(id).await;
        backend.spawn_exec(id, command, tty).await
    }

    /// Return the main process PID of a running container.
    ///
    /// Reads the pid file that the OCI runtime wrote at `create` time.
    /// Returns `None` if the pid file is missing or unparseable.
    pub fn container_pid(&self, id: &str) -> Option<u32> {
        let pid_file = self.data_dir.join("logs").join(id).join("container.pid");
        std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|s| s.trim().parse().ok())
    }

    /// Query the real OCI runtime state of a container.
    pub async fn container_state(&self, id: &str) -> Result<ContainerStateInfo> {
        let backend = self.get_backend_for_container(id).await;
        backend.state(id).await
    }

    /// Full cleanup: stop + delete + remove from store + cleanup container dir.
    pub async fn cleanup_container(&self, id: &str) -> Result<()> {
        let backend = self.get_backend_for_container(id).await;
        // Best-effort stop and delete
        let _ = backend.stop(id).await;
        let _ = backend.delete(id).await;

        // Remove from store
        self.store.remove(id);

        // Clean up container directory
        let container_dir = self.data_dir.join("containers").join(id);
        if container_dir.exists() {
            tokio::fs::remove_dir_all(&container_dir).await?;
        }

        info!("Container {} cleaned up", id);
        Ok(())
    }

    // ─── Image Management ───────────────────────────────────────

    pub async fn list_images(&self) -> Result<Vec<crate::image::ImageInfo>> {
        self.image_manager.list_images().await
    }

    pub async fn delete_image(&self, image_id: &str) -> Result<()> {
        self.image_manager.delete_image(image_id).await
    }
}
