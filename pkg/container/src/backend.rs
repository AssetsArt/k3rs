use anyhow::Result;
use async_trait::async_trait;
use std::path::{Path, PathBuf};

use crate::state::ContainerStateInfo;

/// Pluggable runtime backend trait.
/// Implementations: Virtualization (macOS), OCI (youki/crun on Linux).
#[async_trait]
pub trait RuntimeBackend: Send + Sync {
    /// Human-readable name of this runtime backend.
    fn name(&self) -> &str;

    /// Version string of the runtime.
    fn version(&self) -> &str;

    /// Create a container from an OCI bundle directory (OCI) or image name (Docker).
    async fn create(&self, id: &str, bundle: &Path) -> Result<()>;

    /// Create a container directly from an image reference (Docker shortcut).
    /// Default implementation delegates to create() — Docker overrides this.
    async fn create_from_image(&self, id: &str, image: &str, command: &[String]) -> Result<()> {
        let _ = (id, image, command);
        Err(anyhow::anyhow!(
            "create_from_image not supported by this backend",
        ))
    }

    /// Start a created container.
    async fn start(&self, id: &str) -> Result<()>;

    /// Stop a running container (SIGTERM → SIGKILL).
    async fn stop(&self, id: &str) -> Result<()>;

    /// Delete a stopped container.
    async fn delete(&self, id: &str) -> Result<()>;

    /// List running container IDs managed by k3rs.
    async fn list(&self) -> Result<Vec<String>>;

    /// Get logs from a container.
    async fn logs(&self, id: &str, tail: usize) -> Result<Vec<String>>;

    /// Execute a command inside a running container.
    async fn exec(&self, id: &str, command: &[&str]) -> Result<String>;

    /// Spawn a command inside a running container and return the child process handle.
    /// Used for interactive WebSocket sessions.
    /// `tty` — allocate a pseudo-terminal inside the container (like `docker exec -t`).
    async fn spawn_exec(
        &self,
        id: &str,
        command: &[&str],
        tty: bool,
    ) -> Result<tokio::process::Child>;

    /// Query the real OCI runtime state of a container.
    /// Runs `<runtime> state <id>` and parses the JSON output.
    async fn state(&self, id: &str) -> Result<ContainerStateInfo> {
        // Default: not implemented — subclasses override
        Err(anyhow::anyhow!(
            "state query not supported by backend for container {}",
            id
        ))
    }

    /// Whether this backend handles image pulling internally (e.g. Docker).
    fn handles_images(&self) -> bool {
        false
    }
}

// ─── OCI Backend ────────────────────────────────────────────────

/// OCI-compliant runtime backend — invokes youki or crun.
/// No mocking — every method is a real subprocess call to the OCI runtime.
pub struct OciBackend {
    /// Path to the OCI runtime binary (e.g. /usr/bin/youki)
    runtime_path: String,
    /// Name of the detected runtime
    runtime_name: String,
    /// Version string
    runtime_version: String,
    /// Directory for container log files
    log_dir: PathBuf,
    /// Root directory for runtime state (--root flag)
    state_dir: PathBuf,
}

impl OciBackend {
    /// Auto-detect an OCI runtime in $PATH.
    /// Priority: youki or crun
    pub fn detect(data_dir: &std::path::Path) -> Result<Self> {
        for name in &["youki", "crun"] {
            if let Ok(oci) = Self::with_name(name, data_dir) {
                return Ok(oci);
            }
        }
        Err(anyhow::anyhow!(
            "No OCI runtime found in PATH. Install youki or crun.",
        ))
    }

    /// Create an OciBackend for a specific runtime name (e.g. "youki", "crun")
    pub fn with_name(name: &str, data_dir: &std::path::Path) -> Result<Self> {
        if let Ok(output) = std::process::Command::new("which").arg(name).output()
            && output.status.success()
        {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let version = Self::get_version(&path);
            tracing::info!("Using OCI runtime: {} at {} ({})", name, path, version);
            return Ok(Self {
                runtime_path: path,
                runtime_name: name.to_string(),
                runtime_version: version,
                log_dir: data_dir.join("logs"),
                state_dir: data_dir.join("state"),
            });
        }
        Err(anyhow::anyhow!("OCI runtime {} not found in PATH", name))
    }

    /// Create with an explicit runtime path.
    pub fn new(runtime_path: &str, data_dir: &std::path::Path) -> Self {
        let name = std::path::Path::new(runtime_path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let version = Self::get_version(runtime_path);
        Self {
            runtime_path: runtime_path.to_string(),
            runtime_name: name,
            runtime_version: version,
            log_dir: data_dir.join("logs"),
            state_dir: data_dir.join("state"),
        }
    }

    /// Get the log directory for a container.
    pub fn container_log_dir(&self, id: &str) -> PathBuf {
        self.log_dir.join(id)
    }

    /// Get the stdout log path for a container.
    pub fn container_log_path(&self, id: &str) -> PathBuf {
        self.log_dir.join(id).join("stdout.log")
    }

    /// Get the PID file path for a container.
    pub fn container_pid_file(&self, id: &str) -> PathBuf {
        self.log_dir.join(id).join("container.pid")
    }

    /// Read the PID from the PID file, if it exists.
    pub fn read_pid(&self, id: &str) -> Option<u32> {
        let pid_path = self.container_pid_file(id);
        std::fs::read_to_string(&pid_path)
            .ok()
            .and_then(|s| s.trim().parse().ok())
    }

    /// Ensure the log directory exists for a container.
    pub fn ensure_log_dir(&self, id: &str) -> Result<PathBuf> {
        let dir = self.container_log_dir(id);
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    fn get_version(path: &str) -> String {
        std::process::Command::new(path)
            .arg("--version")
            .output()
            .ok()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .next()
                    .unwrap_or("unknown")
                    .trim()
                    .to_string()
            })
            .unwrap_or_else(|| "unknown".to_string())
    }

    fn cmd(&self) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(&self.runtime_path);
        // Use a custom root directory for state — avoids permission issues
        cmd.arg("--root").arg(&self.state_dir);
        cmd
    }
}

#[async_trait]
impl RuntimeBackend for OciBackend {
    fn name(&self) -> &str {
        &self.runtime_name
    }

    fn version(&self) -> &str {
        &self.runtime_version
    }

    async fn create(&self, id: &str, bundle: &Path) -> Result<()> {
        tracing::info!("[{}] create container: {}", self.runtime_name, id);

        // Ensure log directory and state directory exist
        self.ensure_log_dir(id)?;
        std::fs::create_dir_all(&self.state_dir).map_err(|e| {
            tracing::warn!("[{}] failed to create state dir: {}", self.runtime_name, e);
            e
        })?;

        let pid_file = self.container_pid_file(id);
        let log_path = self.container_log_path(id);
        let runtime_log = self.container_log_dir(id).join("runtime.log");

        // Ensure the log directory exists
        self.ensure_log_dir(id)?;

        // Open the stdout log file for the container process
        let stdout_file = std::fs::File::options()
            .create(true)
            .append(true)
            .open(&log_path)?;
        let stderr_file = stdout_file.try_clone()?;

        let status = self
            .cmd()
            .args([
                "--log",
                &runtime_log.to_string_lossy(),
                "create",
                "--bundle",
                &bundle.to_string_lossy(),
                "--pid-file",
                &pid_file.to_string_lossy(),
                id,
            ])
            .stdout(stdout_file)
            .stderr(stderr_file)
            .status()
            .await?;

        if !status.success() {
            let stderr = tokio::fs::read_to_string(&log_path)
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            anyhow::bail!(
                "[{}] create failed for {}: {}",
                self.runtime_name,
                id,
                stderr.trim()
            );
        }

        tracing::info!(
            "[{}] container {} created (pid-file: {}, log: {})",
            self.runtime_name,
            id,
            pid_file.display(),
            log_path.display()
        );
        Ok(())
    }

    async fn start(&self, id: &str) -> Result<()> {
        tracing::info!("[{}] start container: {}", self.runtime_name, id);
        let output = self.cmd().args(["start", id]).output().await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!(
                "[{}] start failed for {}: {}",
                self.runtime_name,
                id,
                stderr.trim()
            );
        }

        // Log the PID if captured
        if let Some(pid) = self.read_pid(id) {
            tracing::info!(
                "[{}] container {} started with PID {}",
                self.runtime_name,
                id,
                pid
            );
        }

        Ok(())
    }

    async fn stop(&self, id: &str) -> Result<()> {
        tracing::info!("[{}] stop container: {}", self.runtime_name, id);

        // Send SIGTERM first
        let _ = self.cmd().args(["kill", id, "SIGTERM"]).output().await;

        // Wait briefly then force kill
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        let _ = self.cmd().args(["kill", id, "SIGKILL"]).output().await;

        Ok(())
    }

    async fn delete(&self, id: &str) -> Result<()> {
        tracing::info!("[{}] delete container: {}", self.runtime_name, id);
        let output = self.cmd().args(["delete", "--force", id]).output().await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!(
                "[{}] delete warning for {}: {}",
                self.runtime_name,
                id,
                stderr.trim()
            );
        }

        // Clean up log/pid files
        let log_dir = self.container_log_dir(id);
        if log_dir.exists() {
            let _ = std::fs::remove_dir_all(&log_dir);
        }

        Ok(())
    }

    async fn list(&self) -> Result<Vec<String>> {
        let output = self.cmd().args(["list", "-q"]).output().await?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let ids: Vec<String> = stdout
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.trim().to_string())
            .collect();
        Ok(ids)
    }

    async fn logs(&self, id: &str, tail: usize) -> Result<Vec<String>> {
        // Read from the container's stdout log file.
        let log_path = self.container_log_path(id);
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
                "[{}] No logs available for container {} (log path: {})",
                self.runtime_name,
                id,
                log_path.display()
            )]),
        }
    }

    async fn exec(&self, id: &str, command: &[&str]) -> Result<String> {
        tracing::info!(
            "[{}] exec in container {}: {:?}",
            self.runtime_name,
            id,
            command
        );

        let mut args = vec!["exec", id];
        args.extend_from_slice(command);

        let output = self.cmd().args(&args).output().await?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if output.status.success() {
            Ok(stdout)
        } else {
            anyhow::bail!(
                "[{}] exec failed in {}: {}{}",
                self.runtime_name,
                id,
                stdout,
                stderr
            )
        }
    }

    async fn spawn_exec(
        &self,
        id: &str,
        command: &[&str],
        tty: bool,
    ) -> Result<tokio::process::Child> {
        tracing::info!(
            "[{}] spawning interactive exec in container {}: {:?} (tty={})",
            self.runtime_name,
            id,
            command,
            tty
        );

        let mut args = vec!["exec"];
        // --tty allocates a pseudo-terminal inside the container so the shell
        // gets a proper terminal (shows prompts, handles Ctrl+C, etc.).
        if tty {
            args.push("--tty");
        }
        args.push(id);
        if command.is_empty() {
            args.push("/bin/sh");
        } else {
            args.extend_from_slice(command);
        }

        let child = self
            .cmd()
            .args(&args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;

        Ok(child)
    }

    async fn state(&self, id: &str) -> Result<ContainerStateInfo> {
        let output = self.cmd().args(["state", id]).output().await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!(
                "[{}] state query failed for {}: {}",
                self.runtime_name,
                id,
                stderr.trim()
            );
        }

        let stdout = String::from_utf8_lossy(&output.stdout);

        // Parse the OCI runtime state JSON
        // Format: { "ociVersion": "...", "id": "...", "status": "...", "pid": N, "bundle": "..." }
        let state: serde_json::Value = serde_json::from_str(&stdout).map_err(|e| {
            anyhow::anyhow!(
                "[{}] failed to parse state JSON for {}: {} (raw: {})",
                self.runtime_name,
                id,
                e,
                stdout.trim()
            )
        })?;

        Ok(ContainerStateInfo {
            id: state
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or(id)
                .to_string(),
            status: state
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string(),
            pid: state.get("pid").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            bundle: state
                .get("bundle")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        })
    }
}
