use anyhow::Result;
use async_trait::async_trait;
use std::path::Path;

/// Pluggable runtime backend trait.
/// Implementations: Docker (macOS), OCI (youki/crun/runc on Linux), Stub (dev).
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
            "create_from_image not supported by this backend"
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

    /// Whether this backend handles image pulling internally (e.g. Docker).
    fn handles_images(&self) -> bool {
        false
    }
}

// ─── Stub Backend ───────────────────────────────────────────────

/// Dev/test backend that only logs operations. No real containers.
pub struct StubBackend;

#[async_trait]
impl RuntimeBackend for StubBackend {
    fn name(&self) -> &str {
        "stub"
    }

    fn version(&self) -> &str {
        "dev"
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

    async fn exec(&self, id: &str, command: &[&str]) -> Result<String> {
        tracing::info!("[stub] exec in container {}: {:?}", id, command);
        // Run the command on the host as a simulation
        let cmd_str = command.first().unwrap_or(&"echo");
        let args = if command.len() > 1 {
            &command[1..]
        } else {
            &[]
        };
        match tokio::process::Command::new(cmd_str)
            .args(args)
            .output()
            .await
        {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                if output.status.success() {
                    Ok(stdout)
                } else {
                    Ok(format!("{}{}", stdout, stderr))
                }
            }
            Err(e) => Ok(format!("exec error: {}", e)),
        }
    }
}

// ─── Docker Backend ─────────────────────────────────────────────

/// Docker CLI backend — wraps `docker` commands for macOS development.
pub struct DockerBackend {
    docker_path: String,
    docker_version: String,
}

impl DockerBackend {
    /// Detect Docker by running `docker info`.
    pub fn detect() -> Result<Self> {
        // Find docker binary
        let which_output = std::process::Command::new("which").arg("docker").output()?;
        if !which_output.status.success() {
            anyhow::bail!("docker not found in PATH");
        }
        let docker_path = String::from_utf8_lossy(&which_output.stdout)
            .trim()
            .to_string();

        // Check Docker daemon is running
        let info_output = std::process::Command::new(&docker_path)
            .args(["info", "--format", "{{.ServerVersion}}"])
            .output()?;
        if !info_output.status.success() {
            let stderr = String::from_utf8_lossy(&info_output.stderr);
            anyhow::bail!("Docker daemon not running: {}", stderr.trim());
        }

        let docker_version = String::from_utf8_lossy(&info_output.stdout)
            .trim()
            .to_string();
        tracing::info!(
            "Detected Docker: {} (version {})",
            docker_path,
            docker_version
        );

        Ok(Self {
            docker_path,
            docker_version,
        })
    }

    fn cmd(&self) -> std::process::Command {
        std::process::Command::new(&self.docker_path)
    }

    fn container_name(id: &str) -> String {
        format!("k3rs-{}", &id[..12.min(id.len())])
    }
}

#[async_trait]
impl RuntimeBackend for DockerBackend {
    fn name(&self) -> &str {
        "docker"
    }

    fn version(&self) -> &str {
        &self.docker_version
    }

    async fn create(&self, id: &str, _bundle: &Path) -> Result<()> {
        // Docker doesn't use OCI bundles — use create_from_image instead
        tracing::warn!(
            "[docker] create() called with bundle — use create_from_image() instead. id={}",
            id
        );
        Ok(())
    }

    async fn create_from_image(&self, id: &str, image: &str, command: &[String]) -> Result<()> {
        let name = Self::container_name(id);
        tracing::info!("[docker] creating container {} from image {}", name, image);

        let mut args = vec![
            "create".to_string(),
            "--name".to_string(),
            name.clone(),
            "--label".to_string(),
            "k3rs.managed=true".to_string(),
            "--label".to_string(),
            format!("k3rs.pod-id={}", id),
        ];

        args.push(image.to_string());

        // Add command if specified
        for c in command {
            args.push(c.clone());
        }

        let output = self.cmd().args(&args).output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // If container already exists, that's OK
            if stderr.contains("is already in use") {
                tracing::warn!("[docker] container {} already exists", name);
                return Ok(());
            }
            anyhow::bail!("[docker] create failed: {}", stderr.trim());
        }

        tracing::info!("[docker] container {} created", name);
        Ok(())
    }

    async fn start(&self, id: &str) -> Result<()> {
        let name = Self::container_name(id);
        tracing::info!("[docker] starting container {}", name);

        let output = self.cmd().args(["start", &name]).output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("[docker] start failed: {}", stderr.trim());
        }
        Ok(())
    }

    async fn stop(&self, id: &str) -> Result<()> {
        let name = Self::container_name(id);
        tracing::info!("[docker] stopping container {}", name);

        let _ = self.cmd().args(["stop", "-t", "5", &name]).output();
        let _ = self.cmd().args(["rm", "-f", &name]).output();
        Ok(())
    }

    async fn delete(&self, id: &str) -> Result<()> {
        let name = Self::container_name(id);
        let _ = self.cmd().args(["rm", "-f", &name]).output();
        Ok(())
    }

    async fn list(&self) -> Result<Vec<String>> {
        let output = self
            .cmd()
            .args([
                "ps",
                "-q",
                "--filter",
                "label=k3rs.managed=true",
                "--format",
                "{{.Label \"k3rs.pod-id\"}}",
            ])
            .output()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let ids: Vec<String> = stdout
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.trim().to_string())
            .collect();
        Ok(ids)
    }

    async fn logs(&self, id: &str, tail: usize) -> Result<Vec<String>> {
        let name = Self::container_name(id);
        let output = self
            .cmd()
            .args(["logs", "--tail", &tail.to_string(), &name])
            .output()?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let combined = format!("{}{}", stdout, stderr);
        let lines: Vec<String> = combined.lines().map(|l| l.to_string()).collect();
        Ok(lines)
    }

    async fn exec(&self, id: &str, command: &[&str]) -> Result<String> {
        let name = Self::container_name(id);
        tracing::info!("[docker] exec in container {}: {:?}", name, command);

        let mut args = vec!["exec", &name];
        args.extend_from_slice(command);

        let output = self.cmd().args(&args).output()?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if output.status.success() {
            Ok(stdout)
        } else {
            Ok(format!("{}{}", stdout, stderr))
        }
    }

    fn handles_images(&self) -> bool {
        true // Docker pulls images internally
    }
}

// ─── OCI Backend ────────────────────────────────────────────────

/// OCI-compliant runtime backend — invokes youki, crun, or runc.
pub struct OciBackend {
    /// Path to the OCI runtime binary (e.g. /usr/bin/youki)
    runtime_path: String,
    /// Name of the detected runtime
    runtime_name: String,
    /// Version string
    runtime_version: String,
}

impl OciBackend {
    /// Auto-detect an OCI runtime in $PATH.
    /// Priority: youki → crun → runc
    pub fn detect() -> Result<Self> {
        for name in &["youki", "crun", "runc"] {
            if let Ok(output) = std::process::Command::new("which").arg(name).output() {
                if output.status.success() {
                    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    let version = Self::get_version(&path);
                    tracing::info!("Detected OCI runtime: {} at {} ({})", name, path, version);
                    return Ok(Self {
                        runtime_path: path,
                        runtime_name: name.to_string(),
                        runtime_version: version,
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
        let version = Self::get_version(runtime_path);
        Self {
            runtime_path: runtime_path.to_string(),
            runtime_name: name,
            runtime_version: version,
        }
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

    fn cmd(&self) -> std::process::Command {
        std::process::Command::new(&self.runtime_path)
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

    async fn exec(&self, id: &str, command: &[&str]) -> Result<String> {
        tracing::info!(
            "[{}] exec in container {}: {:?}",
            self.runtime_name,
            id,
            command
        );

        let mut args = vec!["exec", id];
        args.extend_from_slice(command);

        let output = self.cmd().args(&args).output()?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if output.status.success() {
            Ok(stdout)
        } else {
            if stderr.contains("does not support exec") {
                // Some runtimes (older crun) may not support exec
                anyhow::bail!("{} does not support exec", self.runtime_name);
            }
            Ok(format!("{}{}", stdout, stderr))
        }
    }
}
