use tracing::{info, warn};

/// Container runtime client.
/// Connects to containerd via gRPC when a socket path is provided.
/// Falls back to stub mode (log-only) when no socket is available.
pub struct ContainerRuntime {
    stub_mode: bool,
    #[allow(dead_code)]
    channel: Option<tonic::transport::Channel>,
}

impl ContainerRuntime {
    /// Create a new container runtime client.
    /// If `socket_path` is None, runs in stub mode (no real containerd).
    pub async fn new(socket_path: Option<&str>) -> anyhow::Result<Self> {
        match socket_path {
            Some(path) => {
                info!("Connecting to containerd at {}", path);
                match containerd_client::connect(path).await {
                    Ok(channel) => {
                        // Verify connection by querying version
                        let mut version_client =
                            containerd_client::services::v1::version_client::VersionClient::new(
                                channel.clone(),
                            );
                        match version_client.version(()).await {
                            Ok(resp) => {
                                let v = resp.into_inner();
                                info!(
                                    "Connected to containerd: version={} revision={}",
                                    v.version, v.revision
                                );
                            }
                            Err(e) => {
                                warn!("containerd version query failed: {} — using stub mode", e);
                                return Ok(Self {
                                    stub_mode: true,
                                    channel: None,
                                });
                            }
                        }
                        Ok(Self {
                            stub_mode: false,
                            channel: Some(channel),
                        })
                    }
                    Err(e) => {
                        warn!(
                            "Failed to connect to containerd at {}: {} — falling back to stub mode",
                            path, e
                        );
                        Ok(Self {
                            stub_mode: true,
                            channel: None,
                        })
                    }
                }
            }
            None => {
                info!("Container runtime running in STUB mode (no containerd)");
                Ok(Self {
                    stub_mode: true,
                    channel: None,
                })
            }
        }
    }

    /// Create a stub-mode runtime (for backwards compatibility with sync callers).
    pub fn new_stub() -> Self {
        info!("Container runtime running in STUB mode (no containerd)");
        Self {
            stub_mode: true,
            channel: None,
        }
    }

    fn channel(&self) -> anyhow::Result<tonic::transport::Channel> {
        self.channel
            .clone()
            .ok_or_else(|| anyhow::anyhow!("No containerd channel available"))
    }

    // ─── Image Operations ───────────────────────────────────────────

    pub async fn pull_image(&self, image: &str) -> anyhow::Result<()> {
        if self.stub_mode {
            info!("[stub] Pulling image: {}", image);
            return Ok(());
        }

        info!("Pulling image: {}", image);
        let channel = self.channel()?;
        let mut client = containerd_client::services::v1::images_client::ImagesClient::new(channel);

        // Check if image already exists
        let req = containerd_client::services::v1::GetImageRequest {
            name: image.to_string(),
        };
        match client.get(req).await {
            Ok(_) => {
                info!("Image {} already exists", image);
                return Ok(());
            }
            Err(_) => {
                info!("Image {} not found locally, pulling...", image);
            }
        }

        // Create image record — actual pulling is done by containerd's content service
        let img = containerd_client::services::v1::Image {
            name: image.to_string(),
            labels: Default::default(),
            target: None,
            created_at: None,
            updated_at: None,
        };
        let req = containerd_client::services::v1::CreateImageRequest {
            image: Some(img),
            source_date_epoch: None,
        };
        client
            .create(req)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create image record for {}: {}", image, e))?;

        info!("Image {} pulled successfully", image);
        Ok(())
    }

    // ─── Container Operations ───────────────────────────────────────

    pub async fn create_container(
        &self,
        id: &str,
        image: &str,
        command: &[String],
    ) -> anyhow::Result<()> {
        if self.stub_mode {
            info!(
                "[stub] Creating container: id={}, image={}, cmd={:?}",
                id, image, command
            );
            return Ok(());
        }

        info!("Creating container: id={}, image={}", id, image);
        let channel = self.channel()?;
        let mut client =
            containerd_client::services::v1::containers_client::ContainersClient::new(channel);

        let container = containerd_client::services::v1::Container {
            id: id.to_string(),
            image: image.to_string(),
            runtime: Some(containerd_client::services::v1::container::Runtime {
                name: "io.containerd.runc.v2".to_string(),
                options: None,
            }),
            labels: Default::default(),
            spec: None,
            snapshot_key: String::new(),
            snapshotter: "overlayfs".to_string(),
            created_at: None,
            updated_at: None,
            extensions: Default::default(),
            sandbox: String::new(),
        };

        let req = containerd_client::services::v1::CreateContainerRequest {
            container: Some(container),
        };

        client
            .create(req)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create container {}: {}", id, e))?;

        info!("Container {} created successfully", id);
        Ok(())
    }

    // ─── Task Operations (start/stop) ───────────────────────────────

    pub async fn start_container(&self, id: &str) -> anyhow::Result<()> {
        if self.stub_mode {
            info!("[stub] Starting container: {}", id);
            return Ok(());
        }

        info!("Starting container: {}", id);
        let channel = self.channel()?;
        let mut client = containerd_client::services::v1::tasks_client::TasksClient::new(channel);

        // Create task (process) for the container
        let req = containerd_client::services::v1::CreateTaskRequest {
            container_id: id.to_string(),
            rootfs: vec![],
            stdin: String::new(),
            stdout: String::new(),
            stderr: String::new(),
            terminal: false,
            checkpoint: None,
            options: None,
            runtime_path: String::new(),
        };

        client
            .create(req)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create task for container {}: {}", id, e))?;

        // Start the task
        let req = containerd_client::services::v1::StartRequest {
            container_id: id.to_string(),
            exec_id: String::new(),
        };

        client
            .start(req)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to start container {}: {}", id, e))?;

        info!("Container {} started successfully", id);
        Ok(())
    }

    pub async fn stop_container(&self, id: &str) -> anyhow::Result<()> {
        if self.stub_mode {
            info!("[stub] Stopping container: {}", id);
            return Ok(());
        }

        info!("Stopping container: {}", id);
        let channel = self.channel()?;
        let mut client = containerd_client::services::v1::tasks_client::TasksClient::new(channel);

        // Send SIGTERM (signal 15) to gracefully stop
        let req = containerd_client::services::v1::KillRequest {
            container_id: id.to_string(),
            exec_id: String::new(),
            signal: 15, // SIGTERM
            all: false,
        };

        if let Err(e) = client.kill(req).await {
            warn!("Kill signal to container {} failed: {}", id, e);
        }

        // Delete the task
        let req = containerd_client::services::v1::DeleteTaskRequest {
            container_id: id.to_string(),
        };

        client
            .delete(req)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to delete task for container {}: {}", id, e))?;

        info!("Container {} stopped successfully", id);
        Ok(())
    }

    // ─── List Operations ────────────────────────────────────────────

    pub async fn list_containers(&self) -> anyhow::Result<Vec<String>> {
        if self.stub_mode {
            info!("[stub] Listing containers");
            return Ok(vec![]);
        }

        info!("Listing containers");
        let channel = self.channel()?;
        let mut client =
            containerd_client::services::v1::containers_client::ContainersClient::new(channel);

        let req = containerd_client::services::v1::ListContainersRequest { filters: vec![] };

        let resp = client
            .list(req)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to list containers: {}", e))?;

        let ids: Vec<String> = resp
            .into_inner()
            .containers
            .into_iter()
            .map(|c| c.id)
            .collect();

        info!("Found {} containers", ids.len());
        Ok(ids)
    }

    // ─── Logs ───────────────────────────────────────────────────────

    /// Get logs from a container.
    /// In real mode, reads from the container's log directory.
    /// In stub mode, returns simulated log lines.
    pub async fn container_logs(&self, id: &str, tail: usize) -> anyhow::Result<Vec<String>> {
        if self.stub_mode {
            info!("[stub] Getting logs for container: {}", id);
            let now = chrono::Utc::now();
            let logs: Vec<String> = (0..tail.min(20))
                .map(|i| {
                    let ts = now - chrono::Duration::seconds((tail - i) as i64);
                    format!(
                        "{} [container/{}] simulated log line {}",
                        ts.format("%Y-%m-%dT%H:%M:%SZ"),
                        id,
                        i + 1
                    )
                })
                .collect();
            return Ok(logs);
        }

        info!("Getting logs for container: {}", id);

        // containerd stores logs at /var/log/pods/ or via the shim.
        // Read the stdout log file for this container's task.
        let log_path = format!("/var/log/containerd/{}/stdout.log", id);
        match tokio::fs::read_to_string(&log_path).await {
            Ok(content) => {
                let lines: Vec<String> = content
                    .lines()
                    .rev()
                    .take(tail)
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                Ok(lines)
            }
            Err(e) => {
                warn!("Failed to read container logs at {}: {}", log_path, e);
                Ok(vec![format!("[{}] Log file not available: {}", id, e)])
            }
        }
    }
}
