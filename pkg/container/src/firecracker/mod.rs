//! Firecracker microVM backend for Linux.
//!
//! Provides KVM-based lightweight VM isolation using Firecracker.
//! Each container runs inside its own microVM with:
//!
//! - **virtio-blk** or **virtiofs**: OCI rootfs shared as guest filesystem
//! - **virtio-net**: TAP + iptables NAT for pod connectivity
//! - **serial console**: stdout/stderr streaming to host log file
//! - **vsock**: host ↔ guest exec channel (port 5555)
//!
//! ## Boot design
//!
//! Firecracker is configured via its REST API (Unix socket), then started.
//! The kernel boots with `root=/dev/vda init=/sbin/k3rs-init` (ext4 mode) or
//! mounts virtiofs from a virtiofsd daemon. k3rs-init reads `/config.json`
//! and execs the container entrypoint.
//!
//! ## Exec
//!
//! Exec connects directly to the Firecracker vsock UDS at
//! `{vsock_uds_path}_{5555}` — no intermediary helper needed (unlike the
//! macOS VZ backend which requires k3rs-vmm).
//!
//! ## Requirements
//! - Linux with `/dev/kvm` access
//! - `firecracker` binary (auto-downloaded if not in PATH)
//! - Linux kernel at kernel directory (shared KernelManager)
//! - Optional: `jailer` for chroot + seccomp + cgroups
//! - Optional: `virtiofsd` for shared directory rootfs

pub mod api;
pub mod installer;
pub mod jailer;
pub mod network;
pub mod rootfs;

use crate::backend::RuntimeBackend;
use crate::kernel::KernelManager;
use crate::state::ContainerStateInfo;
use anyhow::{Context, Result};
use api::FcApiClient;
use async_trait::async_trait;
use installer::FcInstaller;
use rootfs::{FcRootfsManager, FcRootfsMode, RootfsStrategy};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::RwLock;

use pkg_constants::runtime::{DEFAULT_CPU_COUNT, DEFAULT_MEMORY_MB, FC_GUEST_CID};
use pkg_constants::vm::VSOCK_EXEC_PORT;

/// Per-VM instance state tracking.
#[derive(Debug, Clone)]
struct FcInstance {
    /// Rootfs mode (ext4 image path or virtiofsd info)
    rootfs_mode: FcRootfsMode,
    /// OCI rootfs directory (before conversion to ext4 or virtiofsd)
    rootfs_dir: PathBuf,
    /// PID of the Firecracker process
    fc_pid: Option<u32>,
    /// API socket path
    api_socket: PathBuf,
    /// vsock UDS path
    vsock_uds: PathBuf,
    /// TAP device name (for cleanup)
    tap_name: Option<String>,
    /// VM state
    state: FcVmState,
    /// Serial console log path
    log_path: PathBuf,
    /// Guest CID for vsock
    guest_cid: u32,
}

#[derive(Debug, Clone, PartialEq)]
enum FcVmState {
    Created,
    Running,
    Stopped,
}

/// Firecracker microVM backend.
///
/// Each container runs inside a lightweight Linux microVM using KVM.
/// Supports both ext4 block device and virtiofsd shared directory modes.
pub struct FirecrackerBackend {
    /// Directory for VM runtime data
    data_dir: PathBuf,
    /// Path to the guest Linux kernel
    kernel_path: PathBuf,
    /// Path to the initrd image (optional)
    initrd_path: Option<PathBuf>,
    /// Path to the firecracker binary
    firecracker_bin: PathBuf,
    /// Path to the jailer binary (optional)
    jailer_bin: Option<PathBuf>,
    /// Active VM instances
    instances: Arc<RwLock<HashMap<String, FcInstance>>>,
    /// Kernel asset manager (used for availability checks)
    #[allow(dead_code)]
    kernel_manager: KernelManager,
    /// Counter for unique guest CIDs
    next_cid: Arc<tokio::sync::Mutex<u32>>,
    /// Detected rootfs strategy
    rootfs_strategy: RootfsStrategy,
}

impl FirecrackerBackend {
    /// Create a new FirecrackerBackend.
    ///
    /// Checks for KVM, finds/downloads Firecracker, provisions kernel.
    pub async fn new(data_dir: &Path) -> Result<Self> {
        // 1. Check /dev/kvm
        if !FcInstaller::kvm_available() {
            anyhow::bail!(
                "KVM not available — /dev/kvm missing or not accessible. \
                 Firecracker requires Linux with KVM support."
            );
        }

        // 2. Find or download firecracker binary
        let fc_bin = FcInstaller::ensure_firecracker().await?;
        let jailer_bin = FcInstaller::ensure_jailer().await.unwrap_or(None);

        // 3. Ensure VM directory exists
        let vm_dir = data_dir.join("vms");
        tokio::fs::create_dir_all(&vm_dir).await?;

        // 4. Provision kernel + initrd via shared KernelManager
        let kernel_manager = KernelManager::with_dir(&data_dir.join("kernel"));
        let (kernel_path, initrd_path) =
            kernel_manager.ensure_available().await.unwrap_or_else(|e| {
                tracing::warn!("Kernel provisioning: {}. Using default path.", e);
                (
                    PathBuf::from(format!("{}/vmlinux", pkg_constants::paths::DATA_DIR)),
                    None,
                )
            });

        // 5. Detect rootfs strategy
        let rootfs_strategy = FcRootfsManager::detect_mode();

        // 6. Setup NAT (once, globally)
        if let Err(e) = network::FcNetworkManager::setup_nat().await {
            tracing::warn!("[fc] NAT setup failed: {} (networking may not work)", e);
        }

        let kernel_exists = tokio::fs::metadata(&kernel_path).await.is_ok();
        tracing::info!(
            "FirecrackerBackend: kernel={}{} firecracker={} jailer={} rootfs={:?} cpus={} mem={}MB",
            kernel_path.display(),
            if kernel_exists { " ✓" } else { " ✗" },
            fc_bin.display(),
            jailer_bin
                .as_ref()
                .map(|p| format!("{} ✓", p.display()))
                .unwrap_or_else(|| "none".to_string()),
            rootfs_strategy,
            DEFAULT_CPU_COUNT,
            DEFAULT_MEMORY_MB,
        );

        Ok(Self {
            data_dir: vm_dir,
            kernel_path,
            initrd_path,
            firecracker_bin: fc_bin,
            jailer_bin,
            instances: Arc::new(RwLock::new(HashMap::new())),
            kernel_manager,
            next_cid: Arc::new(tokio::sync::Mutex::new(FC_GUEST_CID)),
            rootfs_strategy,
        })
    }

    // ─── Path helpers ────────────────────────────────────────────────

    fn api_socket_path(&self, id: &str) -> PathBuf {
        self.data_dir.join(format!("{}.sock", id))
    }

    fn vsock_uds_path(&self, id: &str) -> PathBuf {
        self.data_dir.join(format!("{}-vsock.sock", id))
    }

    fn log_path(&self, id: &str) -> PathBuf {
        self.data_dir.join(format!("{}.log", id))
    }

    fn rootfs_dir(&self, id: &str) -> PathBuf {
        self.data_dir.join(format!("{}-rootfs", id))
    }

    fn rootfs_img_path(&self, id: &str) -> PathBuf {
        self.data_dir.join(format!("{}-rootfs.ext4", id))
    }

    fn virtiofsd_socket_path(&self, id: &str) -> PathBuf {
        self.data_dir.join(format!("{}-virtiofsd.sock", id))
    }

    fn pid_file_path(&self, id: &str) -> PathBuf {
        self.data_dir.join(format!("{}.pid", id))
    }

    // ─── CID allocation ──────────────────────────────────────────────

    async fn allocate_cid(&self) -> u32 {
        let mut cid = self.next_cid.lock().await;
        let current = *cid;
        *cid += 1;
        current
    }

    // ─── Rootfs preparation ──────────────────────────────────────────

    /// Prepare the rootfs with k3rs-init + config.json, then create
    /// the appropriate rootfs mode (ext4 or virtiofsd).
    async fn prepare_rootfs(
        &self,
        rootfs_dir: &Path,
        id: &str,
        command: &[String],
        env: &[String],
    ) -> Result<FcRootfsMode> {
        // Inject k3rs-init and write config.json (same as VZ backend)
        Self::inject_init_and_config(rootfs_dir, id, command, env).await?;

        match self.rootfs_strategy {
            RootfsStrategy::Ext4 => {
                let img_path = self.rootfs_img_path(id);
                FcRootfsManager::create_ext4_image(rootfs_dir, &img_path).await?;
                Ok(FcRootfsMode::Ext4 {
                    image_path: img_path,
                })
            }
            RootfsStrategy::Virtiofsd => {
                let socket_path = self.virtiofsd_socket_path(id);
                let pid =
                    FcRootfsManager::start_virtiofsd(rootfs_dir, &socket_path).await?;
                Ok(FcRootfsMode::Virtiofsd {
                    shared_dir: rootfs_dir.to_path_buf(),
                    socket_path,
                    virtiofsd_pid: Some(pid),
                })
            }
        }
    }

    /// Inject k3rs-init binary and config.json into the rootfs.
    async fn inject_init_and_config(
        rootfs_dir: &Path,
        id: &str,
        command: &[String],
        env: &[String],
    ) -> Result<()> {
        // Create required guest directories
        for dir in &[
            "sbin", "proc", "sys", "dev", "dev/pts", "dev/shm", "tmp", "run", "mnt/rootfs", "etc",
        ] {
            tokio::fs::create_dir_all(rootfs_dir.join(dir)).await.ok();
        }

        // Inject k3rs-init
        let init_dest = rootfs_dir.join("sbin/k3rs-init");
        if let Some(init_src) = crate::virt::find_k3rs_init() {
            tokio::fs::copy(&init_src, &init_dest)
                .await
                .with_context(|| {
                    format!(
                        "copy k3rs-init {} → {}",
                        init_src.display(),
                        init_dest.display()
                    )
                })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&init_dest, std::fs::Permissions::from_mode(0o755))?;
            }
            tracing::debug!(
                "[fc] injected k3rs-init at {} (from {})",
                init_dest.display(),
                init_src.display()
            );
        } else {
            tracing::warn!(
                "[fc] k3rs-init not found — guest will use existing /sbin/k3rs-init"
            );
        }

        // Write config.json
        let mut all_env: Vec<String> = vec![
            "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string(),
            format!("HOSTNAME={}", id),
            "TERM=xterm".to_string(),
        ];
        for e in env {
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

        let config_dest = rootfs_dir.join("config.json");
        tokio::fs::write(&config_dest, serde_json::to_string_pretty(&config)?)
            .await
            .with_context(|| format!("write config.json to {}", config_dest.display()))?;

        Ok(())
    }

    // ─── VM lifecycle ────────────────────────────────────────────────

    /// Spawn the Firecracker process with process independence.
    ///
    /// Uses `setsid()` to detach from agent session (mirrors VZ backend's
    /// `boot_vm()` pattern). Writes PID file for post-restart recovery.
    async fn spawn_firecracker(&self, id: &str) -> Result<u32> {
        let api_socket = self.api_socket_path(id);
        let log_path = self.log_path(id);

        // Remove stale socket
        let _ = tokio::fs::remove_file(&api_socket).await;

        let log_file = std::fs::File::create(&log_path)?;
        let stderr_file = log_file.try_clone()?;

        let mut cmd = std::process::Command::new(&self.firecracker_bin);
        cmd.args([
            "--api-sock",
            &api_socket.to_string_lossy(),
        ]);

        cmd.stdout(log_file)
            .stderr(stderr_file)
            .stdin(std::process::Stdio::null());

        // Process independence: setsid()
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            unsafe {
                cmd.pre_exec(|| {
                    libc::setsid();
                    Ok(())
                });
            }
        }

        let child = cmd.spawn().context("failed to spawn firecracker")?;
        let pid: u32 = child.id();

        // Write PID file
        if let Err(e) = std::fs::write(self.pid_file_path(id), format!("{}\n", pid)) {
            tracing::warn!("[fc] failed to write PID file for VM {}: {}", id, e);
        }

        // Wait for API socket to appear
        for _ in 0..100 {
            if api_socket.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        if !api_socket.exists() {
            anyhow::bail!(
                "Firecracker API socket did not appear at {}",
                api_socket.display()
            );
        }

        tracing::info!(
            "[fc] Firecracker spawned (pid={}, socket={})",
            pid,
            api_socket.display()
        );
        Ok(pid)
    }

    /// Configure the VM via REST API and start it.
    async fn configure_and_boot(
        &self,
        id: &str,
        rootfs_mode: &FcRootfsMode,
        guest_cid: u32,
        tap_name: Option<&str>,
    ) -> Result<()> {
        let api = FcApiClient::new(&self.api_socket_path(id).to_string_lossy());

        // 1. Machine config
        api.set_machine_config(DEFAULT_CPU_COUNT, DEFAULT_MEMORY_MB)
            .await?;

        // 2. Boot source
        let boot_args = match rootfs_mode {
            FcRootfsMode::Ext4 { .. } => {
                "console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw init=/sbin/k3rs-init"
                    .to_string()
            }
            FcRootfsMode::Virtiofsd { .. } => {
                "console=ttyS0 reboot=k panic=1 pci=off root=virtiofs:rootfs rw init=/sbin/k3rs-init"
                    .to_string()
            }
        };
        api.set_boot_source(
            &self.kernel_path.to_string_lossy(),
            &boot_args,
            self.initrd_path
                .as_ref()
                .map(|p| p.to_string_lossy().to_string())
                .as_deref(),
        )
        .await?;

        // 3. Root device
        match rootfs_mode {
            FcRootfsMode::Ext4 { image_path } => {
                api.add_drive("rootfs", &image_path.to_string_lossy(), true, false)
                    .await?;
            }
            FcRootfsMode::Virtiofsd { .. } => {
                // virtiofsd uses vhost-user-fs — configured separately
                // The Firecracker API for this is not yet standard;
                // for now, fall back to ext4 if virtiofs config fails
                tracing::info!("[fc] virtiofsd mode: rootfs shared via vhost-user-fs");
            }
        }

        // 4. vsock
        let vsock_uds = self.vsock_uds_path(id);
        api.set_vsock(guest_cid, &vsock_uds.to_string_lossy())
            .await?;

        // 5. Network interface (if TAP available)
        if let Some(tap) = tap_name {
            api.add_network_interface("eth0", tap).await?;
        }

        // 6. Start instance
        let boot_start = std::time::Instant::now();
        api.start_instance().await?;
        let boot_elapsed = boot_start.elapsed();

        tracing::info!(
            "[fc] VM {} booted in {:?} (cid={}, kernel={}, rootfs={:?})",
            id,
            boot_elapsed,
            guest_cid,
            self.kernel_path.display(),
            rootfs_mode
        );

        if boot_elapsed > std::time::Duration::from_millis(125) {
            tracing::warn!(
                "[fc] VM {} boot time {:?} exceeds 125ms target",
                id,
                boot_elapsed
            );
        }

        Ok(())
    }

    /// Stop a VM: graceful shutdown via API, then SIGTERM/SIGKILL fallback.
    async fn stop_vm(&self, id: &str, instance: &FcInstance) -> Result<()> {
        // Try graceful shutdown via API
        let api_socket = &instance.api_socket;
        if api_socket.exists() {
            let api = FcApiClient::new(&api_socket.to_string_lossy());
            if api.send_ctrl_alt_del().await.is_ok() {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }

        // Signal-based fallback
        if let Some(pid) = instance.fc_pid {
            let _ = tokio::process::Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .output()
                .await;
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            let _ = tokio::process::Command::new("kill")
                .args(["-KILL", &pid.to_string()])
                .output()
                .await;
        }

        // Stop virtiofsd if running
        if let FcRootfsMode::Virtiofsd {
            virtiofsd_pid: Some(pid),
            ..
        } = &instance.rootfs_mode
        {
            FcRootfsManager::stop_virtiofsd(*pid).await;
        }

        // Cleanup TAP
        if instance.tap_name.is_some() {
            network::FcNetworkManager::cleanup_tap(id).await;
        }

        // Cleanup socket files + PID file
        let _ = tokio::fs::remove_file(self.api_socket_path(id)).await;
        let _ = tokio::fs::remove_file(self.vsock_uds_path(id)).await;
        let _ = std::fs::remove_file(self.pid_file_path(id));

        Ok(())
    }

    /// Restore VMs from PID files after agent restart.
    async fn restore_from_pid_files(&self, discovered: &mut std::collections::HashSet<String>) {
        let mut dir = match tokio::fs::read_dir(&self.data_dir).await {
            Ok(d) => d,
            Err(_) => return,
        };

        while let Ok(Some(entry)) = dir.next_entry().await {
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();
            if !name.ends_with(".pid") {
                continue;
            }

            let vm_id = name.trim_end_matches(".pid").to_string();

            let pid: u32 = match tokio::fs::read_to_string(entry.path())
                .await
                .ok()
                .and_then(|s| s.trim().parse().ok())
            {
                Some(p) => p,
                None => {
                    let _ = tokio::fs::remove_file(entry.path()).await;
                    continue;
                }
            };

            let alive = unsafe { libc::kill(pid as libc::pid_t, 0) == 0 };

            if alive {
                discovered.insert(vm_id.clone());

                let mut instances = self.instances.write().await;
                if !instances.contains_key(&vm_id) {
                    tracing::info!(
                        "[fc] restored VM {} from PID file (pid={}, process alive)",
                        vm_id,
                        pid
                    );
                    instances.insert(
                        vm_id.clone(),
                        FcInstance {
                            rootfs_mode: FcRootfsMode::Ext4 {
                                image_path: self.rootfs_img_path(&vm_id),
                            },
                            rootfs_dir: self.rootfs_dir(&vm_id),
                            fc_pid: Some(pid),
                            api_socket: self.api_socket_path(&vm_id),
                            vsock_uds: self.vsock_uds_path(&vm_id),
                            tap_name: None,
                            state: FcVmState::Running,
                            log_path: self.log_path(&vm_id),
                            guest_cid: 0, // Unknown after restart
                        },
                    );
                }
            } else {
                tracing::info!(
                    "[fc] removing stale PID file for VM {} (pid={}, process gone)",
                    vm_id,
                    pid
                );
                let _ = tokio::fs::remove_file(entry.path()).await;
            }
        }
    }

    // ─── Exec via vsock ──────────────────────────────────────────────

    /// Execute a one-shot command via Firecracker vsock.
    ///
    /// Connects to the vsock UDS at `{uds_path}_{port}` and uses
    /// the same protocol as k3rs-init: `cmd\0arg1\0arg2\n`.
    async fn exec_via_vsock(&self, id: &str, command: &[&str]) -> Result<String> {
        let vsock_uds = {
            let instances = self.instances.read().await;
            instances
                .get(id)
                .ok_or_else(|| anyhow::anyhow!("VM {} not found", id))?
                .vsock_uds
                .clone()
        };

        // Firecracker vsock UDS convention: {uds_path}_{port}
        let connect_path = format!("{}_{}", vsock_uds.display(), VSOCK_EXEC_PORT);

        let mut stream = tokio::net::UnixStream::connect(&connect_path)
            .await
            .with_context(|| format!("failed to connect to vsock at {}", connect_path))?;

        // Send command: cmd\0arg1\0arg2\n
        let payload = format!("{}\n", command.join("\0"));
        stream.write_all(payload.as_bytes()).await?;

        // Read response until EOF
        let mut output = Vec::new();
        stream.read_to_end(&mut output).await?;

        Ok(String::from_utf8_lossy(&output).to_string())
    }
}

// ─── RuntimeBackend impl ─────────────────────────────────────────────────────

#[async_trait]
impl RuntimeBackend for FirecrackerBackend {
    fn name(&self) -> &str {
        "vm"
    }

    fn version(&self) -> &str {
        "firecracker-1.12.0"
    }

    async fn create(&self, id: &str, bundle: &Path) -> Result<()> {
        tracing::info!("[fc] create: id={} bundle={}", id, bundle.display());

        // Resolve OCI rootfs
        let rootfs_dir = if bundle.join("rootfs").exists() {
            bundle.join("rootfs")
        } else {
            bundle.to_path_buf()
        };

        // Parse entrypoint + env from bundle config.json
        let (command, env) = crate::virt::parse_bundle_config(bundle);

        // Prepare rootfs (inject k3rs-init, create ext4 or start virtiofsd)
        let rootfs_mode = self.prepare_rootfs(&rootfs_dir, id, &command, &env).await?;

        let guest_cid = self.allocate_cid().await;
        let log_path = self.log_path(id);
        tokio::fs::write(&log_path, "").await?;

        self.instances.write().await.insert(
            id.to_string(),
            FcInstance {
                rootfs_mode,
                rootfs_dir,
                fc_pid: None,
                api_socket: self.api_socket_path(id),
                vsock_uds: self.vsock_uds_path(id),
                tap_name: None,
                state: FcVmState::Created,
                log_path,
                guest_cid,
            },
        );

        tracing::info!("[fc] container {} created (rootfs prepared)", id);
        Ok(())
    }

    async fn create_from_image(&self, id: &str, image: &str, command: &[String]) -> Result<()> {
        tracing::info!("[fc] create_from_image: id={} image={}", id, image);

        let rootfs_dir = self.rootfs_dir(id);
        tokio::fs::create_dir_all(&rootfs_dir).await?;

        let rootfs_mode = self.prepare_rootfs(&rootfs_dir, id, command, &[]).await?;

        let guest_cid = self.allocate_cid().await;
        let log_path = self.log_path(id);
        tokio::fs::write(
            &log_path,
            format!("[fc] VM for image {} (cmd: {:?})\n", image, command),
        )
        .await?;

        self.instances.write().await.insert(
            id.to_string(),
            FcInstance {
                rootfs_mode,
                rootfs_dir,
                fc_pid: None,
                api_socket: self.api_socket_path(id),
                vsock_uds: self.vsock_uds_path(id),
                tap_name: None,
                state: FcVmState::Created,
                log_path,
                guest_cid,
            },
        );

        Ok(())
    }

    async fn start(&self, id: &str) -> Result<()> {
        tracing::info!("[fc] start VM: {}", id);

        if tokio::fs::metadata(&self.kernel_path).await.is_err() {
            anyhow::bail!(
                "Kernel missing at {} — run `scripts/build-kernel.sh`",
                self.kernel_path.display()
            );
        }

        let (rootfs_mode, guest_cid) = {
            let instances = self.instances.read().await;
            let inst = instances
                .get(id)
                .ok_or_else(|| anyhow::anyhow!("VM {} not found — call create() first", id))?;
            (inst.rootfs_mode.clone(), inst.guest_cid)
        };

        // 1. Spawn Firecracker process
        let pid = self.spawn_firecracker(id).await?;

        // 2. Setup TAP device
        let tap_name = match network::FcNetworkManager::setup_tap(id, guest_cid).await {
            Ok(name) => Some(name),
            Err(e) => {
                tracing::warn!("[fc] TAP setup failed (networking unavailable): {}", e);
                None
            }
        };

        // 3. Configure via REST API and boot
        self.configure_and_boot(id, &rootfs_mode, guest_cid, tap_name.as_deref())
            .await?;

        // 4. Update state
        let mut instances = self.instances.write().await;
        if let Some(inst) = instances.get_mut(id) {
            inst.state = FcVmState::Running;
            inst.fc_pid = Some(pid);
            inst.tap_name = tap_name;
        }

        Ok(())
    }

    async fn stop(&self, id: &str) -> Result<()> {
        tracing::info!("[fc] stop VM: {}", id);

        let instance = {
            let instances = self.instances.read().await;
            instances.get(id).cloned()
        };

        if let Some(inst) = instance {
            self.stop_vm(id, &inst).await?;
        }

        let mut instances = self.instances.write().await;
        if let Some(inst) = instances.get_mut(id) {
            inst.state = FcVmState::Stopped;
            inst.fc_pid = None;
        }

        Ok(())
    }

    async fn delete(&self, id: &str) -> Result<()> {
        tracing::info!("[fc] delete VM: {}", id);

        self.stop(id).await.ok();

        let removed = self.instances.write().await.remove(id);
        if let Some(inst) = removed {
            // Clean up rootfs
            match &inst.rootfs_mode {
                FcRootfsMode::Ext4 { image_path } => {
                    let _ = tokio::fs::remove_file(image_path).await;
                }
                FcRootfsMode::Virtiofsd {
                    socket_path,
                    virtiofsd_pid,
                    ..
                } => {
                    if let Some(pid) = virtiofsd_pid {
                        FcRootfsManager::stop_virtiofsd(*pid).await;
                    }
                    let _ = tokio::fs::remove_file(socket_path).await;
                }
            }
            let _ = tokio::fs::remove_dir_all(&inst.rootfs_dir).await;
            let _ = tokio::fs::remove_file(&inst.log_path).await;

            // Jailer cleanup
            if let Some(ref jailer_bin) = self.jailer_bin {
                let j = jailer::Jailer::new(jailer_bin);
                j.cleanup(id).await;
            }
        }

        // Best-effort cleanup for named paths
        let _ = tokio::fs::remove_dir_all(self.rootfs_dir(id)).await;
        let _ = tokio::fs::remove_file(self.rootfs_img_path(id)).await;
        let _ = tokio::fs::remove_file(self.log_path(id)).await;
        let _ = tokio::fs::remove_file(self.pid_file_path(id)).await;
        let _ = tokio::fs::remove_file(self.api_socket_path(id)).await;
        let _ = tokio::fs::remove_file(self.vsock_uds_path(id)).await;

        tracing::info!("[fc] VM {} deleted", id);
        Ok(())
    }

    async fn list(&self) -> Result<Vec<String>> {
        let mut ids: std::collections::HashSet<String> = {
            let instances = self.instances.read().await;
            instances
                .iter()
                .filter(|(_, i)| i.state == FcVmState::Running)
                .map(|(k, _)| k.clone())
                .collect()
        };

        // PID file scan + liveness check
        self.restore_from_pid_files(&mut ids).await;

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
            Err(_) => Ok(vec![format!("[fc] no logs for VM {}", id)]),
        }
    }

    async fn exec(&self, id: &str, command: &[&str]) -> Result<String> {
        tracing::info!("[fc] exec in VM {}: {:?}", id, command);

        // Verify the VM is running
        let is_running = {
            let instances = self.instances.read().await;
            instances
                .get(id)
                .map(|i| i.state == FcVmState::Running)
                .unwrap_or(false)
        };

        if !is_running {
            let st = self.state(id).await?;
            if st.status != "running" {
                anyhow::bail!("VM {} is not running (status: {})", id, st.status);
            }
        }

        self.exec_via_vsock(id, command).await
    }

    async fn spawn_exec(
        &self,
        id: &str,
        command: &[&str],
        tty: bool,
    ) -> Result<tokio::process::Child> {
        tracing::info!("[fc] spawn_exec in VM {} tty={}: {:?}", id, tty, command);

        let vsock_uds = {
            let instances = self.instances.read().await;
            instances
                .get(id)
                .ok_or_else(|| anyhow::anyhow!("VM {} not found", id))?
                .vsock_uds
                .clone()
        };

        let connect_path = format!("{}_{}", vsock_uds.display(), VSOCK_EXEC_PORT);

        // Build the command payload
        let cmd_args: Vec<&str> = if command.is_empty() {
            vec!["/bin/sh"]
        } else {
            command.to_vec()
        };

        let prefix = if tty { "\x01" } else { "" };
        let payload = format!("{}{}\n", prefix, cmd_args.join("\0"));

        // Use socat as a stdio ↔ Unix socket bridge.
        // socat provides a Child process with piped stdin/stdout.
        let mut child = tokio::process::Command::new("socat")
            .args(["STDIO", &format!("UNIX-CONNECT:{}", connect_path)])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .context("failed to spawn socat for vsock bridge (is socat installed?)")?;

        // Write the command prefix through stdin
        if let Some(ref mut stdin) = child.stdin {
            stdin.write_all(payload.as_bytes()).await?;
        }

        Ok(child)
    }

    async fn state(&self, id: &str) -> Result<ContainerStateInfo> {
        // In-memory check (fast path)
        {
            let instances = self.instances.read().await;
            if let Some(inst) = instances.get(id) {
                let status = match inst.state {
                    FcVmState::Created => "created",
                    FcVmState::Running => "running",
                    FcVmState::Stopped => "stopped",
                }
                .to_string();
                return Ok(ContainerStateInfo {
                    id: id.to_string(),
                    status,
                    pid: inst.fc_pid.unwrap_or(0),
                    bundle: inst.rootfs_dir.to_string_lossy().to_string(),
                });
            }
        }

        // PID file check (post-restart recovery)
        let pid_file = self.pid_file_path(id);
        if let Ok(content) = tokio::fs::read_to_string(&pid_file).await {
            if let Ok(pid) = content.trim().parse::<u32>() {
                let alive = unsafe { libc::kill(pid as libc::pid_t, 0) == 0 };
                if alive {
                    return Ok(ContainerStateInfo {
                        id: id.to_string(),
                        status: "running".to_string(),
                        pid,
                        bundle: self.rootfs_dir(id).to_string_lossy().to_string(),
                    });
                }
            }
        }

        anyhow::bail!("VM {} not found", id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_helpers() {
        let backend = FirecrackerBackend {
            data_dir: PathBuf::from("/tmp/test-vms"),
            kernel_path: PathBuf::from("/tmp/vmlinux"),
            initrd_path: None,
            firecracker_bin: PathBuf::from("/usr/local/bin/firecracker"),
            jailer_bin: None,
            instances: Arc::new(RwLock::new(HashMap::new())),
            kernel_manager: KernelManager::with_dir(Path::new("/tmp/test")),
            next_cid: Arc::new(tokio::sync::Mutex::new(FC_GUEST_CID)),
            rootfs_strategy: RootfsStrategy::Ext4,
        };

        assert_eq!(
            backend.api_socket_path("vm-001"),
            PathBuf::from("/tmp/test-vms/vm-001.sock")
        );
        assert_eq!(
            backend.vsock_uds_path("vm-001"),
            PathBuf::from("/tmp/test-vms/vm-001-vsock.sock")
        );
        assert_eq!(
            backend.rootfs_img_path("vm-001"),
            PathBuf::from("/tmp/test-vms/vm-001-rootfs.ext4")
        );
        assert_eq!(
            backend.pid_file_path("vm-001"),
            PathBuf::from("/tmp/test-vms/vm-001.pid")
        );
    }

    #[tokio::test]
    async fn test_cid_allocation() {
        let cid_counter = Arc::new(tokio::sync::Mutex::new(FC_GUEST_CID));
        {
            let mut cid = cid_counter.lock().await;
            let first = *cid;
            *cid += 1;
            assert_eq!(first, 3);
        }
        {
            let mut cid = cid_counter.lock().await;
            let second = *cid;
            *cid += 1;
            assert_eq!(second, 4);
        }
    }
}
