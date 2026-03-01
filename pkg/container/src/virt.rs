//! Virtualization.framework microVM backend for macOS.
//!
//! Provides Firecracker-like lightweight Linux VM isolation using Apple's
//! native Virtualization.framework. Each container/pod runs inside its own
//! isolated microVM with virtio devices:
//!
//! - **virtio-fs**: OCI rootfs shared as guest filesystem (no disk images)
//! - **virtio-net**: NAT networking for pod connectivity
//! - **virtio-console**: stdout/stderr log streaming to host file
//! - **virtio-vsock**: host ↔ guest exec channel (port 5555)
//!
//! ## Boot design
//!
//! The kernel is launched with `root=virtiofs:rootfs rw init=/sbin/k3rs-init`.
//! No separate initrd is required. Before booting, this backend injects
//! the `k3rs-init` binary into the OCI rootfs at `sbin/init` and writes a
//! `config.json` at the rootfs root. The kernel mounts the virtiofs share
//! as `/` and runs `k3rs-init`, which reads `config.json` and execs the
//! container's entrypoint from the OCI image.
//!
//! ## Exec
//!
//! Exec is forwarded via the k3rs-vmm helper's IPC socket. The IPC handler
//! in the boot process connects to the guest's vsock listener (port 5555),
//! sends the NUL-delimited command, and returns stdout+stderr.
//!
//! ## Requirements
//! - macOS 13+ (Ventura)
//! - `k3rs-vmm` binary in PATH or `./target/`
//! - Linux kernel at `/var/lib/k3rs/vmlinux`
//! - `k3rs-init` (aarch64/x86_64 Linux musl) at `/var/lib/k3rs/k3rs-init`
//!   or `./target/<arch>-unknown-linux-musl/release/k3rs-init`

use anyhow::{Context, Result};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::backend::RuntimeBackend;
use crate::kernel::KernelManager;
use crate::state::ContainerStateInfo;

use pkg_constants::paths::DATA_DIR;

/// Path where k3rs-init is injected inside the guest rootfs.
const GUEST_INIT_PATH: &str = "sbin/k3rs-init";
/// Standard config.json path inside guest rootfs (read by k3rs-init).
const GUEST_CONFIG_PATH: &str = "config.json";
use pkg_constants::runtime::{DEFAULT_CPU_COUNT, DEFAULT_MEMORY_MB};

/// Per-VM resource configuration.
#[derive(Debug, Clone)]
pub struct VmConfig {
    /// Number of vCPUs
    pub cpu_count: u32,
    /// Memory in megabytes
    pub memory_mb: u64,
}

impl Default for VmConfig {
    fn default() -> Self {
        Self {
            cpu_count: DEFAULT_CPU_COUNT,
            memory_mb: DEFAULT_MEMORY_MB,
        }
    }
}

/// VM instance state tracking.
#[derive(Debug, Clone)]
struct VmInstance {
    /// Path to the rootfs directory shared via virtio-fs
    rootfs_dir: PathBuf,
    /// PID of the k3rs-vmm boot process
    vmm_pid: Option<u32>,
    state: VmState,
    log_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
enum VmState {
    Created,
    Running,
    Stopped,
}

/// Apple Virtualization.framework backend.
///
/// Each container runs inside a lightweight Linux microVM using virtio-fs
/// to share the OCI rootfs directly (no disk image conversion needed).
pub struct VirtualizationBackend {
    /// Directory for VM runtime data (rootfs dirs, logs)
    data_dir: PathBuf,
    /// Path to the guest Linux kernel
    kernel_path: PathBuf,
    /// Path to the initrd image (k3rs-init runs as PID 1 from initrd)
    initrd_path: Option<PathBuf>,
    /// Per-VM resource configuration
    vm_config: VmConfig,
    /// Active VM instances (in-memory, repopulated on discovery)
    instances: Arc<RwLock<HashMap<String, VmInstance>>>,
    /// Kernel asset manager
    kernel_manager: KernelManager,
}

impl VirtualizationBackend {
    /// Create a new VirtualizationBackend with default configuration.
    pub async fn new(data_dir: &Path) -> Result<Self> {
        #[cfg(not(target_os = "macos"))]
        {
            anyhow::bail!("VirtualizationBackend requires macOS");
        }

        #[cfg(target_os = "macos")]
        {
            let vm_dir = data_dir.join("vms");
            tokio::fs::create_dir_all(&vm_dir).await?;

            let kernel_manager = KernelManager::with_dir(&data_dir.join("kernel"));
            let (kernel_path, initrd_path) =
                kernel_manager.ensure_available().await.unwrap_or_else(|e| {
                    tracing::warn!("Kernel provisioning: {}. Using default path.", e);
                    (PathBuf::from("/var/lib/k3rs/vmlinux"), None)
                });

            let kernel_exists = tokio::fs::metadata(&kernel_path).await.is_ok();
            let init_exists = find_k3rs_init().is_some();

            tracing::info!(
                "VirtualizationBackend: kernel={}{} k3rs-init={} cpus={} mem={}MB",
                kernel_path.display(),
                if kernel_exists {
                    " ✓"
                } else {
                    " ✗ (run scripts/build-kernel.sh)"
                },
                if init_exists {
                    "✓"
                } else {
                    "✗ (run scripts/build-kernel.sh)"
                },
                DEFAULT_CPU_COUNT,
                DEFAULT_MEMORY_MB,
            );

            Ok(Self {
                data_dir: vm_dir,
                kernel_path,
                initrd_path,
                vm_config: VmConfig::default(),
                instances: Arc::new(RwLock::new(HashMap::new())),
                kernel_manager,
            })
        }
    }

    /// Create with custom VM resource configuration.
    pub async fn with_config(data_dir: &Path, config: VmConfig) -> Result<Self> {
        let mut backend = Self::new(data_dir).await?;
        backend.vm_config = config;
        Ok(backend)
    }

    fn log_path(&self, id: &str) -> PathBuf {
        self.data_dir.join(format!("{}.log", id))
    }

    fn rootfs_dir(&self, id: &str) -> PathBuf {
        self.data_dir.join(format!("{}-rootfs", id))
    }

    /// Prepare a rootfs directory so the VM kernel can boot it directly.
    ///
    /// The kernel cmdline `root=virtiofs:rootfs init=/sbin/k3rs-init` causes Linux
    /// to mount the virtiofs share as `/` and execute `/sbin/k3rs-init`. This
    /// method makes that work by:
    ///
    ///  1. Ensuring required guest directories exist.
    ///  2. Injecting `k3rs-init` as `/sbin/k3rs-init` in the rootfs (avoids
    ///     overwriting the container's own `/sbin/k3rs-init`).
    ///  3. Writing `/config.json` (read by k3rs-init to find the entrypoint).
    async fn prepare_rootfs(
        &self,
        rootfs: &Path,
        id: &str,
        command: &[String],
        env: &[String],
    ) -> Result<()> {
        // ── 1. Required guest directories ─────────────────────────────────────
        for dir in &[
            "sbin",
            "proc",
            "sys",
            "dev",
            "dev/pts",
            "dev/shm",
            "tmp",
            "run",
            "mnt/rootfs",
            "etc",
        ] {
            tokio::fs::create_dir_all(rootfs.join(dir)).await.ok();
        }

        // ── 2. Inject k3rs-init as /sbin/k3rs-init ────────────────────────────────
        let init_dest = rootfs.join(GUEST_INIT_PATH); // sbin/init
        match find_k3rs_init() {
            Some(init_src) => {
                tokio::fs::copy(&init_src, &init_dest)
                    .await
                    .with_context(|| {
                        format!(
                            "copy k3rs-init {} → {}",
                            init_src.display(),
                            init_dest.display()
                        )
                    })
                    .map_err(|e| {
                        tracing::error!("Failed to copy k3rs-init: {}", e);
                        e
                    })?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&init_dest, std::fs::Permissions::from_mode(0o755))?;
                }
                tracing::debug!(
                    "[virt] injected k3rs-init at {} (from {})",
                    init_dest.display(),
                    init_src.display()
                );
            }
            None => {
                tracing::warn!(
                    "[virt] k3rs-init not found — guest will use existing /sbin/k3rs-init. \
                     Run `scripts/build-kernel.sh` to build it."
                );
            }
        }

        // ── 3. Write /config.json (k3rs-init reads this to find entrypoint) ──
        let config_dest = rootfs.join(GUEST_CONFIG_PATH); // config.json

        let mut all_env: Vec<String> = vec![
            "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string(),
            format!("HOSTNAME={}", id),
            "TERM=xterm".to_string(),
        ];
        for e in env {
            // Don't duplicate keys
            let key = e.split('=').next().unwrap_or("");
            if !key.is_empty() && !all_env.iter().any(|x| x.starts_with(&format!("{}=", key))) {
                all_env.push(e.clone());
            }
        }

        let args: Vec<&str> = if command.is_empty() {
            vec!["/bin/sh"]
        } else {
            command.iter().map(|s| s.as_str()).collect()
        };

        let config = serde_json::json!({
            "ociVersion": "1.0.0",
            "process": {
                "args": args,
                "env": all_env,
                "cwd": "/"
            },
            "hostname": id
        });

        tokio::fs::write(&config_dest, serde_json::to_string_pretty(&config)?)
            .await
            .with_context(|| format!("write config.json to {}", config_dest.display()))?;

        tracing::debug!("[virt] config.json written to {}", config_dest.display());
        Ok(())
    }

    /// Boot a VM using k3rs-vmm helper.
    ///
    /// When an initrd is available, passes `--initrd` so k3rs-init runs as PID 1
    /// from the initrd and starts the vsock listener + virtiofs mount.
    /// Without initrd, the kernel mounts virtiofs directly as root (requires
    /// CONFIG_VIRTIO_FS=y built into the kernel).
    async fn boot_vm(&self, id: &str, rootfs_dir: &Path) -> Result<Option<u32>> {
        let log_path = self.log_path(id);
        let vmm = which_vmm().await.ok_or_else(|| {
            anyhow::anyhow!("k3rs-vmm not found — build with `cargo build -p k3rs-vmm --release`")
        })?;

        let log_file = std::fs::File::create(&log_path)?;
        let stderr_file = log_file.try_clone()?;

        let mut cmd = std::process::Command::new(&vmm);
        let mut boot_args = vec![
            "boot".to_string(),
            "--kernel".to_string(),
            self.kernel_path.to_string_lossy().to_string(),
            "--rootfs".to_string(),
            rootfs_dir.to_string_lossy().to_string(),
            "--cpus".to_string(),
            self.vm_config.cpu_count.to_string(),
            "--memory".to_string(),
            self.vm_config.memory_mb.to_string(),
            "--id".to_string(),
            id.to_string(),
            "--log".to_string(),
            log_path.to_string_lossy().to_string(),
            "--foreground".to_string(),
        ];
        if let Some(ref initrd) = self.initrd_path {
            boot_args.push("--initrd".to_string());
            boot_args.push(initrd.to_string_lossy().to_string());
        }
        cmd.args(&boot_args).stdout(log_file).stderr(stderr_file);

        let child = cmd.spawn().context("failed to spawn k3rs-vmm")?;
        let pid = child.id();

        tracing::info!(
            "[virt] VM {} booted (pid={}, cpus={}, mem={}MB, rootfs={})",
            id,
            pid,
            self.vm_config.cpu_count,
            self.vm_config.memory_mb,
            rootfs_dir.display()
        );
        Ok(Some(pid))
    }

    /// Stop a VM via k3rs-vmm, falling back to SIGTERM/SIGKILL.
    async fn stop_vm(&self, id: &str, pid: Option<u32>) -> Result<()> {
        if let Some(vmm) = which_vmm().await {
            let out = tokio::process::Command::new(&vmm)
                .args(["stop", "--id", id])
                .output()
                .await;
            if let Ok(o) = out {
                if o.status.success() {
                    tracing::info!("[virt] VM {} stopped via k3rs-vmm", id);
                    return Ok(());
                }
                tracing::warn!(
                    "[virt] k3rs-vmm stop: {}",
                    String::from_utf8_lossy(&o.stderr)
                );
            }
        }

        if let Some(pid) = pid {
            let _ = tokio::process::Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .output()
                .await;
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            let _ = tokio::process::Command::new("kill")
                .args(["-KILL", &pid.to_string()])
                .output()
                .await;
            tracing::info!("[virt] VM {} killed (pid={})", id, pid);
        }

        Ok(())
    }

    /// Execute a command via k3rs-vmm IPC → vsock → guest k3rs-init.
    async fn exec_via_vmm(&self, id: &str, command: &[&str]) -> Result<String> {
        let vmm = which_vmm()
            .await
            .ok_or_else(|| anyhow::anyhow!("k3rs-vmm not found"))?;

        let mut args = vec![
            "exec".to_string(),
            "--id".to_string(),
            id.to_string(),
            "--".to_string(),
        ];
        args.extend(command.iter().map(|s| s.to_string()));

        let output = tokio::process::Command::new(&vmm)
            .args(&args)
            .output()
            .await
            .context("failed to exec via k3rs-vmm")?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if output.status.success() {
            Ok(stdout)
        } else {
            anyhow::bail!("k3rs-vmm exec failed: {}{}", stdout.trim(), stderr.trim())
        }
    }

    /// Check if a guest kernel is available.
    pub async fn kernel_available(&self) -> bool {
        self.kernel_manager.is_available().await
    }
}

// ─── RuntimeBackend impl ─────────────────────────────────────────────────────

#[async_trait]
impl RuntimeBackend for VirtualizationBackend {
    fn name(&self) -> &str {
        "vm"
    }

    fn version(&self) -> &str {
        "macos-vz-2.0"
    }

    /// Create a container from an OCI bundle directory.
    ///
    /// Injects k3rs-init + config.json into the OCI rootfs so the VM kernel
    /// can boot it directly via `root=virtiofs:rootfs init=/sbin/k3rs-init`.
    async fn create(&self, id: &str, bundle: &Path) -> Result<()> {
        tracing::info!("[virt] create: id={} bundle={}", id, bundle.display());

        // Resolve the OCI rootfs (the directory we'll share via virtio-fs)
        let rootfs_dir = if bundle.join("rootfs").exists() {
            bundle.join("rootfs")
        } else {
            bundle.to_path_buf()
        };

        // Extract entrypoint + env from the OCI bundle's config.json
        let (command, env) = parse_bundle_config(bundle);

        // Inject k3rs-init and write /config.json into the rootfs
        self.prepare_rootfs(&rootfs_dir, id, &command, &env).await?;

        let log_path = self.log_path(id);
        tokio::fs::write(&log_path, "").await?;

        self.instances.write().await.insert(
            id.to_string(),
            VmInstance {
                rootfs_dir,
                vmm_pid: None,
                state: VmState::Created,
                log_path,
            },
        );

        tracing::info!(
            "[virt] container {} created (rootfs prepared for virtiofs boot)",
            id
        );
        Ok(())
    }

    /// Create from an image reference (direct shortcut — bypasses image pull).
    async fn create_from_image(&self, id: &str, image: &str, command: &[String]) -> Result<()> {
        tracing::info!("[virt] create_from_image: id={} image={}", id, image);

        let rootfs_dir = self.rootfs_dir(id);
        tokio::fs::create_dir_all(&rootfs_dir).await?;

        self.prepare_rootfs(&rootfs_dir, id, command, &[]).await?;

        let log_path = self.log_path(id);
        tokio::fs::write(
            &log_path,
            format!("[virt] VM for image {} (cmd: {:?})\n", image, command),
        )
        .await?;

        self.instances.write().await.insert(
            id.to_string(),
            VmInstance {
                rootfs_dir,
                vmm_pid: None,
                state: VmState::Created,
                log_path,
            },
        );
        Ok(())
    }

    async fn start(&self, id: &str) -> Result<()> {
        tracing::info!("[virt] start VM: {}", id);

        if !tokio::fs::metadata(&self.kernel_path).await.is_ok() {
            anyhow::bail!(
                "Kernel missing at {} — run `scripts/build-kernel.sh`",
                self.kernel_path.display()
            );
        }

        let rootfs_dir = {
            let instances = self.instances.read().await;
            instances
                .get(id)
                .ok_or_else(|| anyhow::anyhow!("VM {} not found — call create() first", id))?
                .rootfs_dir
                .clone()
        };

        let pid = self.boot_vm(id, &rootfs_dir).await?;

        let mut instances = self.instances.write().await;
        if let Some(inst) = instances.get_mut(id) {
            inst.state = VmState::Running;
            inst.vmm_pid = pid;
        }

        Ok(())
    }

    async fn stop(&self, id: &str) -> Result<()> {
        tracing::info!("[virt] stop VM: {}", id);

        let pid = {
            let instances = self.instances.read().await;
            instances.get(id).and_then(|i| i.vmm_pid)
        };

        self.stop_vm(id, pid).await?;

        let mut instances = self.instances.write().await;
        if let Some(inst) = instances.get_mut(id) {
            inst.state = VmState::Stopped;
            inst.vmm_pid = None;
        }

        Ok(())
    }

    async fn delete(&self, id: &str) -> Result<()> {
        tracing::info!("[virt] delete VM: {}", id);

        self.stop(id).await.ok();

        let removed = self.instances.write().await.remove(id);
        if let Some(inst) = removed {
            let _ = tokio::fs::remove_dir_all(&inst.rootfs_dir).await;
            let _ = tokio::fs::remove_file(&inst.log_path).await;
        }

        // Best-effort cleanup for named paths (handles post-restart recovery cases)
        let _ = tokio::fs::remove_dir_all(self.rootfs_dir(id)).await;
        let _ = tokio::fs::remove_file(self.log_path(id)).await;

        tracing::info!("[virt] VM {} deleted", id);
        Ok(())
    }

    /// List running VMs.
    ///
    /// Combines in-memory state with `k3rs-vmm ls` so VMs running before an
    /// agent restart are rediscovered on startup.
    async fn list(&self) -> Result<Vec<String>> {
        let mut ids: std::collections::HashSet<String> = {
            let instances = self.instances.read().await;
            instances
                .iter()
                .filter(|(_, i)| i.state == VmState::Running)
                .map(|(k, _)| k.clone())
                .collect()
        };

        // Query k3rs-vmm for VMs that survived an agent restart
        if let Some(vmm) = which_vmm().await {
            if let Ok(output) = tokio::process::Command::new(&vmm).arg("ls").output().await {
                let stdout = String::from_utf8_lossy(&output.stdout);
                for line in stdout.lines() {
                    // Skip header / empty lines
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 3 && parts[2] == "running" {
                        let vm_id = parts[0].to_string();
                        ids.insert(vm_id.clone());

                        // Populate in-memory tracking if missing (agent restarted)
                        let mut instances = self.instances.write().await;
                        if !instances.contains_key(&vm_id) {
                            tracing::info!(
                                "[virt] rediscovered VM {} from k3rs-vmm ls (restart recovery)",
                                vm_id
                            );
                            let rootfs_dir = self.rootfs_dir(&vm_id);
                            let log_path = self.log_path(&vm_id);
                            let vmm_pid = parts[1].parse::<u32>().ok();
                            instances.insert(
                                vm_id.clone(),
                                VmInstance {
                                    rootfs_dir,
                                    vmm_pid,
                                    state: VmState::Running,
                                    log_path,
                                },
                            );
                        }
                    }
                }
            }
        }

        Ok(ids.into_iter().collect())
    }

    async fn logs(&self, id: &str, tail: usize) -> Result<Vec<String>> {
        let log_path = self.log_path(id);
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
            Err(_) => Ok(vec![format!("[virt] no logs for VM {}", id)]),
        }
    }

    async fn exec(&self, id: &str, command: &[&str]) -> Result<String> {
        tracing::info!("[virt] exec in VM {}: {:?}", id, command);

        // Verify the VM is running (check in-memory OR via k3rs-vmm state)
        let is_running = {
            let instances = self.instances.read().await;
            instances
                .get(id)
                .map(|i| i.state == VmState::Running)
                .unwrap_or(false)
        };

        if !is_running {
            // May have been discovered after a restart; check live state
            let st = self.state(id).await?;
            if st.status != "running" {
                anyhow::bail!("VM {} is not running (status: {})", id, st.status);
            }
        }

        self.exec_via_vmm(id, command).await
    }

    /// Spawn an interactive exec session.
    ///
    /// Spawns `k3rs-vmm exec --id <id> [--tty] -- <cmd>` as a subprocess and
    /// returns the child handle so the WebSocket exec handler gets piped I/O.
    ///
    /// When `tty=true`, `--tty` is passed to k3rs-vmm exec, which switches it
    /// to streaming mode via IPC → vsock → k3rs-init PTY listener. The guest
    /// shell then runs with a real PTY (prompts, job control, colours).
    async fn spawn_exec(
        &self,
        id: &str,
        command: &[&str],
        tty: bool,
    ) -> Result<tokio::process::Child> {
        tracing::info!("[virt] spawn_exec in VM {} tty={}: {:?}", id, tty, command);

        let vmm = which_vmm().await.ok_or_else(|| {
            anyhow::anyhow!("k3rs-vmm not found — cannot spawn_exec in VM {}", id)
        })?;

        let cmd_args: Vec<&str> = if command.is_empty() {
            vec!["/bin/sh"]
        } else {
            command.to_vec()
        };

        let mut args: Vec<&str> = vec!["exec", "--id", id];
        if tty {
            args.push("--tty");
        }
        args.push("--");
        args.extend_from_slice(&cmd_args);

        let child = tokio::process::Command::new(&vmm)
            .args(&args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .context("failed to spawn k3rs-vmm exec")?;

        Ok(child)
    }

    /// Query the runtime state of a VM.
    ///
    /// First checks in-memory state, then falls back to `k3rs-vmm state --id`
    /// so the agent's `discover_running_containers()` works after restarts.
    async fn state(&self, id: &str) -> Result<ContainerStateInfo> {
        // In-memory check (fast path)
        {
            let instances = self.instances.read().await;
            if let Some(inst) = instances.get(id) {
                let status = match inst.state {
                    VmState::Created => "created",
                    VmState::Running => "running",
                    VmState::Stopped => "stopped",
                }
                .to_string();
                return Ok(ContainerStateInfo {
                    id: id.to_string(),
                    status,
                    pid: inst.vmm_pid.unwrap_or(0),
                    bundle: inst.rootfs_dir.to_string_lossy().to_string(),
                });
            }
        }

        // k3rs-vmm state query (for post-restart recovery)
        if let Some(vmm) = which_vmm().await {
            let out = tokio::process::Command::new(&vmm)
                .args(["state", "--id", id])
                .output()
                .await;

            if let Ok(o) = out {
                let stdout = String::from_utf8_lossy(&o.stdout);
                // Output: "state=running" or "state=not_found"
                if let Some(val) = stdout.lines().find_map(|l| l.strip_prefix("state=")) {
                    let status = if val.trim() == "running" {
                        "running"
                    } else {
                        "stopped"
                    };
                    let pid = if status == "running" {
                        vmm_pid_for(id).await
                    } else {
                        0
                    };
                    return Ok(ContainerStateInfo {
                        id: id.to_string(),
                        status: status.to_string(),
                        pid,
                        bundle: self.rootfs_dir(id).to_string_lossy().to_string(),
                    });
                }
            }
        }

        anyhow::bail!("VM {} not found (not tracked and k3rs-vmm unavailable)", id)
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Find the k3rs-vmm helper binary in PATH or common build locations.
async fn which_vmm() -> Option<String> {
    if let Ok(output) = tokio::process::Command::new("which")
        .arg("k3rs-vmm")
        .output()
        .await
        && output.status.success()
    {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path.is_empty() {
            return Some(path);
        }
    }

    let candidates = [
        "./target/release/k3rs-vmm".to_string(),
        "./target/debug/k3rs-vmm".to_string(),
        format!("{}/bin/k3rs-vmm", DATA_DIR),
        "./k3rs-vmm".to_string(),
    ];
    for path in &candidates {
        if tokio::fs::metadata(path).await.is_ok() {
            return Some(path.to_string());
        }
    }

    None
}

/// Find the k3rs-init Linux binary to inject into the guest rootfs.
///
/// Search order:
/// 1. System install (`/var/lib/k3rs/k3rs-init`) — placed by `scripts/build-kernel.sh`
/// 2. User-local (`~/.k3rs/bin/k3rs-init`)
/// 3. Cargo build output for aarch64 and x86_64 musl targets
pub(crate) fn find_k3rs_init() -> Option<PathBuf> {
    let system_path = format!("{}/bin/k3rs-init", DATA_DIR);
    if let Some(p) = try_path(&system_path) {
        return Some(p);
    }

    if let Some(home) = std::env::var_os("HOME") {
        let user_path = PathBuf::from(home).join(".k3rs/bin/k3rs-init");
        if user_path.exists() {
            return Some(user_path);
        }
    }

    // Cargo build outputs — aarch64 first (Apple Silicon M-series)
    for arch in &["aarch64", "x86_64"] {
        for profile in &["release", "debug"] {
            let p = format!("./target/{}-unknown-linux-musl/{}/k3rs-init", arch, profile);
            if let Some(path) = try_path(&p) {
                return Some(path);
            }
        }
    }

    None
}

fn try_path(s: &str) -> Option<PathBuf> {
    let p = PathBuf::from(s);
    if p.exists() { Some(p) } else { None }
}

/// Parse the OCI bundle config.json to extract command and env vars.
/// Returns empty vecs on parse failure (k3rs-init defaults to /bin/sh).
fn parse_bundle_config(bundle: &Path) -> (Vec<String>, Vec<String>) {
    let data = match std::fs::read_to_string(bundle.join("config.json")) {
        Ok(d) => d,
        Err(_) => return (Vec::new(), Vec::new()),
    };
    let v: serde_json::Value = match serde_json::from_str(&data) {
        Ok(v) => v,
        Err(_) => return (Vec::new(), Vec::new()),
    };

    let command = v["process"]["args"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let env = v["process"]["env"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    (command, env)
}

/// Get the PID of the k3rs-vmm boot process for a given VM ID.
async fn vmm_pid_for(id: &str) -> u32 {
    let out = tokio::process::Command::new("pgrep")
        .args(["-f", &format!("k3rs-vmm boot.*--id {}", id)])
        .output()
        .await;

    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .trim()
            .lines()
            .next()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0),
        _ => 0,
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vm_config_defaults() {
        let config = VmConfig::default();
        assert_eq!(config.cpu_count, 1);
        assert_eq!(config.memory_mb, 256);
    }

    #[test]
    fn test_parse_bundle_config_missing() {
        let (cmd, env) = parse_bundle_config(Path::new("/nonexistent"));
        assert!(cmd.is_empty());
        assert!(env.is_empty());
    }

    #[test]
    fn test_parse_bundle_config_valid() {
        use std::io::Write;
        let tmp = std::env::temp_dir().join("k3rs-virt-cfg-test");
        std::fs::create_dir_all(&tmp).unwrap();

        let cfg = serde_json::json!({
            "process": {
                "args": ["/bin/bash", "-l"],
                "env": ["HOME=/root", "PATH=/bin"]
            }
        });
        let mut f = std::fs::File::create(tmp.join("config.json")).unwrap();
        f.write_all(serde_json::to_string(&cfg).unwrap().as_bytes())
            .unwrap();

        let (cmd, env) = parse_bundle_config(&tmp);
        assert_eq!(cmd, vec!["/bin/bash", "-l"]);
        assert_eq!(env, vec!["HOME=/root", "PATH=/bin"]);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn test_backend_new() {
        let tmp = PathBuf::from("/tmp/k3rs-virt-new-test");
        let _ = tokio::fs::create_dir_all(&tmp).await;
        let backend = VirtualizationBackend::new(&tmp).await.unwrap();
        assert_eq!(backend.name(), "vm");
        assert_eq!(backend.version(), "macos-vz-2.0");
        assert!(!backend.handles_images());
        let _ = tokio::fs::remove_dir_all(&tmp).await;
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn test_create_writes_config_into_rootfs() {
        let tmp = PathBuf::from("/tmp/k3rs-virt-cfg-rootfs-test");
        let _ = tokio::fs::create_dir_all(&tmp).await;
        let backend = VirtualizationBackend::new(&tmp).await.unwrap();

        // Build a fake OCI bundle
        let bundle = tmp.join("bundle");
        let rootfs = bundle.join("rootfs");
        tokio::fs::create_dir_all(&rootfs).await.unwrap();

        let cfg = serde_json::json!({
            "process": {"args": ["/bin/sh"], "env": [], "cwd": "/"}
        });
        tokio::fs::write(
            bundle.join("config.json"),
            serde_json::to_string(&cfg).unwrap(),
        )
        .await
        .unwrap();

        backend.create("vm-cfg-test", &bundle).await.unwrap();

        // config.json must land INSIDE the rootfs (guest reads it at /config.json)
        let guest_cfg = rootfs.join("config.json");
        assert!(
            guest_cfg.exists(),
            "config.json must be written into the rootfs, not just the bundle dir"
        );
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&guest_cfg).unwrap()).unwrap();
        assert_eq!(v["process"]["args"][0], "/bin/sh");

        let _ = tokio::fs::remove_dir_all(&tmp).await;
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn test_no_disk_image_created() {
        let tmp = PathBuf::from("/tmp/k3rs-virt-no-img-test");
        let _ = tokio::fs::create_dir_all(&tmp).await;
        let backend = VirtualizationBackend::new(&tmp).await.unwrap();

        let bundle = tmp.join("bundle2");
        tokio::fs::create_dir_all(bundle.join("rootfs"))
            .await
            .unwrap();

        backend.create("vm-no-img", &bundle).await.unwrap();

        // virtio-fs design: NO disk image files should be created
        let img = tmp.join("vms").join("vm-no-img.img");
        assert!(!img.exists(), "virtio-fs mode must not create .img files");

        let _ = tokio::fs::remove_dir_all(&tmp).await;
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn test_delete_cleans_up() {
        let tmp = PathBuf::from("/tmp/k3rs-virt-del-test");
        let _ = tokio::fs::create_dir_all(&tmp).await;
        let backend = VirtualizationBackend::new(&tmp).await.unwrap();

        backend
            .create_from_image("del-vm-001", "test:latest", &[])
            .await
            .unwrap();

        let rootfs = backend.rootfs_dir("del-vm-001");
        assert!(rootfs.exists());

        backend.delete("del-vm-001").await.unwrap();
        assert!(!rootfs.exists());
        assert!(!backend.instances.read().await.contains_key("del-vm-001"));

        let _ = tokio::fs::remove_dir_all(&tmp).await;
    }
}
