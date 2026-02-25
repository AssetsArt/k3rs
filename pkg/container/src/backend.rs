use anyhow::Result;
use async_trait::async_trait;
use std::path::Path;

/// Pluggable runtime backend trait.
/// Implementations: OCI (youki/crun/runc), Firecracker (future), Stub (dev).
#[async_trait]
pub trait RuntimeBackend: Send + Sync {
    /// Human-readable name of this runtime backend.
    fn name(&self) -> &str;

    /// Create a container from an OCI bundle directory.
    async fn create(&self, id: &str, bundle: &Path) -> Result<()>;

    /// Start a created container.
    async fn start(&self, id: &str) -> Result<()>;

    /// Stop a running container (SIGTERM → SIGKILL).
    async fn stop(&self, id: &str) -> Result<()>;

    /// Delete a stopped container.
    async fn delete(&self, id: &str) -> Result<()>;

    /// List running container IDs.
    async fn list(&self) -> Result<Vec<String>>;

    /// Get logs from a container.
    async fn logs(&self, id: &str, tail: usize) -> Result<Vec<String>>;
}

// ─── Stub Backend ───────────────────────────────────────────────

/// Dev/test backend that only logs operations. No real containers.
pub struct StubBackend;

#[async_trait]
impl RuntimeBackend for StubBackend {
    fn name(&self) -> &str {
        "stub"
    }

    async fn create(&self, id: &str, bundle: &Path) -> Result<()> {
        tracing::info!(
            "[stub] create container: id={}, bundle={}",
            id,
            bundle.display()
        );
        Ok(())
    }

    async fn start(&self, id: &str) -> Result<()> {
        tracing::info!("[stub] start container: {}", id);
        Ok(())
    }

    async fn stop(&self, id: &str) -> Result<()> {
        tracing::info!("[stub] stop container: {}", id);
        Ok(())
    }

    async fn delete(&self, id: &str) -> Result<()> {
        tracing::info!("[stub] delete container: {}", id);
        Ok(())
    }

    async fn list(&self) -> Result<Vec<String>> {
        tracing::info!("[stub] list containers");
        Ok(vec![])
    }

    async fn logs(&self, id: &str, tail: usize) -> Result<Vec<String>> {
        tracing::info!("[stub] logs for container: {} (tail={})", id, tail);
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
        Ok(logs)
    }
}

// ─── OCI Backend ────────────────────────────────────────────────

/// OCI-compliant runtime backend — invokes youki, crun, or runc.
pub struct OciBackend {
    /// Path to the OCI runtime binary (e.g. /usr/bin/youki)
    runtime_path: String,
    /// Name of the detected runtime
    runtime_name: String,
}

impl OciBackend {
    /// Auto-detect an OCI runtime in $PATH.
    /// Priority: youki → crun → runc
    pub fn detect() -> Result<Self> {
        for name in &["youki", "crun", "runc"] {
            if let Ok(output) = std::process::Command::new("which").arg(name).output() {
                if output.status.success() {
                    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    tracing::info!("Detected OCI runtime: {} at {}", name, path);
                    return Ok(Self {
                        runtime_path: path,
                        runtime_name: name.to_string(),
                    });
                }
            }
        }
        Err(anyhow::anyhow!(
            "No OCI runtime found in PATH. Install youki, crun, or runc."
        ))
    }

    /// Create with an explicit runtime path.
    pub fn new(runtime_path: &str) -> Self {
        let name = std::path::Path::new(runtime_path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        Self {
            runtime_path: runtime_path.to_string(),
            runtime_name: name,
        }
    }

    fn cmd(&self) -> std::process::Command {
        std::process::Command::new(&self.runtime_path)
    }
}

#[async_trait]
impl RuntimeBackend for OciBackend {
    fn name(&self) -> &str {
        &self.runtime_name
    }

    async fn create(&self, id: &str, bundle: &Path) -> Result<()> {
        tracing::info!("[{}] create container: {}", self.runtime_name, id);
        let output = self
            .cmd()
            .args(["create", id, "--bundle", &bundle.to_string_lossy()])
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("create failed: {}", stderr);
        }
        Ok(())
    }

    async fn start(&self, id: &str) -> Result<()> {
        tracing::info!("[{}] start container: {}", self.runtime_name, id);
        let output = self.cmd().args(["start", id]).output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("start failed: {}", stderr);
        }
        Ok(())
    }

    async fn stop(&self, id: &str) -> Result<()> {
        tracing::info!("[{}] stop container: {}", self.runtime_name, id);

        // Send SIGTERM first
        let _ = self.cmd().args(["kill", id, "SIGTERM"]).output();

        // Wait briefly then force kill
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        let _ = self.cmd().args(["kill", id, "SIGKILL"]).output();

        Ok(())
    }

    async fn delete(&self, id: &str) -> Result<()> {
        tracing::info!("[{}] delete container: {}", self.runtime_name, id);
        let output = self.cmd().args(["delete", "--force", id]).output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!("delete warning: {}", stderr);
        }
        Ok(())
    }

    async fn list(&self) -> Result<Vec<String>> {
        let output = self.cmd().args(["list", "-q"]).output()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let ids: Vec<String> = stdout
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.trim().to_string())
            .collect();
        Ok(ids)
    }

    async fn logs(&self, id: &str, tail: usize) -> Result<Vec<String>> {
        // OCI runtimes don't have a built-in log command.
        // Logs are read from the container's stdout log file.
        let log_path = format!("/var/run/k3rs/containers/{}/stdout.log", id);
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
            Err(_) => Ok(vec![format!(
                "[{}] No logs available for {}",
                self.runtime_name, id
            )]),
        }
    }
}
