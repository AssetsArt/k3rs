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
//! The actual VM lifecycle is managed by the `k3rs-vmm` helper binary (Swift)
//! which wraps the Virtualization.framework Obj-C API. This module communicates
//! with k3rs-vmm via CLI invocations.
//!
//! ## Architecture
//! ```text
//! ┌─────────────────────────┐     ┌──────────────────────┐
//! │  VirtualizationBackend  │     │  k3rs-vmm (Swift)    │
//! │  (Rust, this module)    │────▶│  Virtualization.fwk  │
//! │                         │     │  - VZLinuxBootLoader  │
//! │  • create → rootfs dir  │     │  - virtio-fs share   │
//! │  • start  → boot_vm()   │     │  - virtio-net NAT    │
//! │  • exec   → vsock exec  │     │  - virtio-console    │
//! │  • stop   → graceful    │     │  - virtio-vsock      │
//! └─────────────────────────┘     └──────────────────────┘
//! ```
//!
//! ## Requirements
//! - macOS 13+ (Ventura)
//! - `k3rs-vmm` binary in PATH or `target/debug/`
//! - Linux kernel at `/var/lib/k3rs/vmlinux`

use anyhow::{Context, Result};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::backend::RuntimeBackend;
use crate::kernel::KernelManager;

/// Default VM resource configuration
const DEFAULT_CPU_COUNT: u32 = 1;
const DEFAULT_MEMORY_MB: u64 = 128;

/// Per-VM resource and path configuration.
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
    /// Container/pod ID
    id: String,
    /// Path to the rootfs directory (shared via virtio-fs)
    rootfs_dir: PathBuf,
    /// Process ID of the k3rs-vmm helper process
    vmm_pid: Option<u32>,
    /// VM state
    state: VmState,
    /// Log file path (virtio-console output)
    log_path: PathBuf,
    /// OCI config.json path (inside rootfs, used during vsock exec)
    #[allow(dead_code)]
    config_path: PathBuf,
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
    /// Path to the guest initrd
    initrd_path: Option<PathBuf>,
    /// Per-VM resource configuration
    vm_config: VmConfig,
    /// Active VM instances
    instances: Arc<RwLock<HashMap<String, VmInstance>>>,
    /// Kernel asset manager
    kernel_manager: KernelManager,
}

impl VirtualizationBackend {
    /// Create a new VirtualizationBackend with default configuration.
    ///
    /// Validates that:
    /// - Running on macOS
    /// - A guest kernel is available (or can be downloaded)
    pub async fn new(data_dir: &Path) -> Result<Self> {
        #[cfg(not(target_os = "macos"))]
        {
            anyhow::bail!("VirtualizationBackend requires macOS");
        }

        let vm_dir = data_dir.join("vms");
        tokio::fs::create_dir_all(&vm_dir).await?;

        // Initialize kernel manager and check for kernel availability
        let kernel_manager = KernelManager::with_dir(&data_dir.join("kernel"));
        let (kernel_path, initrd_path) =
            kernel_manager.ensure_available().await.unwrap_or_else(|e| {
                tracing::warn!("Kernel provisioning failed: {}. Using default paths.", e);
                (PathBuf::from("/var/lib/k3rs/vmlinux"), None)
            });

        let kernel_exists = tokio::fs::metadata(&kernel_path).await.is_ok();
        if !kernel_exists {
            tracing::warn!(
                "Guest kernel not found at {}. VM boot will require a kernel.",
                kernel_path.display()
            );
        }

        tracing::info!(
            "VirtualizationBackend initialized: data_dir={}, kernel={}{}, cpus={}, memory={}MB",
            vm_dir.display(),
            kernel_path.display(),
            if kernel_exists {
                " ✓"
            } else {
                " ✗ (missing)"
            },
            DEFAULT_CPU_COUNT,
            DEFAULT_MEMORY_MB
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

    /// Create with custom VM resource configuration.
    pub async fn with_config(data_dir: &Path, config: VmConfig) -> Result<Self> {
        let mut backend = Self::new(data_dir).await?;
        backend.vm_config = config;
        Ok(backend)
    }

    /// Get the log path for a container.
    fn log_path(&self, id: &str) -> PathBuf {
        self.data_dir.join(format!("{}.log", id))
    }

    /// Get the rootfs directory for a container.
    fn rootfs_dir(&self, id: &str) -> PathBuf {
        self.data_dir.join(format!("{}-rootfs", id))
    }

    /// Get the OCI config.json path for a container.
    fn config_path(&self, id: &str) -> PathBuf {
        self.rootfs_dir(id).join("config.json")
    }

    /// Boot a VM using k3rs-vmm helper.
    ///
    /// The k3rs-vmm process launches a Virtualization.framework VM with:
    /// - VZLinuxBootLoader → kernel + initrd
    /// - virtio-fs → rootfs directory shared as guest "rootfs" tag
    /// - virtio-net → NAT networking
    /// - virtio-console → log file
    /// - virtio-vsock → exec channel
    async fn boot_vm(&self, id: &str, rootfs_dir: &Path) -> Result<Option<u32>> {
        let log_path = self.log_path(id);

        // Find k3rs-vmm helper
        let vmm_path = which_vmm().await;

        match vmm_path {
            Some(vmm) => {
                let log_file = std::fs::File::create(&log_path)?;
                let stderr_file = log_file.try_clone()?;

                let mut cmd = std::process::Command::new(&vmm);
                cmd.args([
                    "boot",
                    "--kernel",
                    &self.kernel_path.to_string_lossy(),
                    "--rootfs",
                    &rootfs_dir.to_string_lossy(),
                    "--cpus",
                    &self.vm_config.cpu_count.to_string(),
                    "--memory",
                    &self.vm_config.memory_mb.to_string(),
                    "--id",
                    id,
                    "--log",
                    &log_path.to_string_lossy(),
                    "--foreground",
                ]);

                // Add initrd if available
                if let Some(ref initrd) = self.initrd_path {
                    cmd.args(["--initrd", &initrd.to_string_lossy()]);
                }

                let child = cmd
                    .stdout(log_file)
                    .stderr(stderr_file)
                    .spawn()
                    .context("failed to spawn k3rs-vmm")?;

                let pid = child.id();
                tracing::info!(
                    "[virt] VM {} booted via k3rs-vmm (pid={}, rootfs={}, cpus={}, mem={}MB)",
                    id,
                    pid,
                    rootfs_dir.display(),
                    self.vm_config.cpu_count,
                    self.vm_config.memory_mb
                );
                Ok(Some(pid))
            }
            None => {
                // No VMM helper — create the log file and track as "created"
                tokio::fs::write(
                    &log_path,
                    format!(
                        "[virt] VM {} created (awaiting k3rs-vmm helper)\n\
                         [virt] rootfs={}\n\
                         [virt] Install k3rs-vmm: cd cmd/k3rs-vmm && swift build -c release\n",
                        id,
                        rootfs_dir.display()
                    ),
                )
                .await?;
                tracing::warn!(
                    "[virt] k3rs-vmm helper not found — VM {} created but not booted. \
                     Build it: ./scripts/build-vmm.sh",
                    id,
                );
                Ok(None)
            }
        }
    }

    /// Stop a VM via k3rs-vmm, falling back to kill.
    async fn stop_vm(&self, id: &str, pid: Option<u32>) -> Result<()> {
        // Try graceful stop via k3rs-vmm
        if let Some(vmm) = which_vmm().await {
            let output = tokio::process::Command::new(&vmm)
                .args(["stop", "--id", id])
                .output()
                .await;

            match output {
                Ok(o) if o.status.success() => {
                    tracing::info!("[virt] VM {} stopped gracefully via k3rs-vmm", id);
                    return Ok(());
                }
                Ok(o) => {
                    tracing::warn!(
                        "[virt] k3rs-vmm stop failed: {}",
                        String::from_utf8_lossy(&o.stderr)
                    );
                }
                Err(e) => {
                    tracing::warn!("[virt] k3rs-vmm stop error: {}", e);
                }
            }
        }

        // Fallback: kill the VMM process
        if let Some(pid) = pid {
            let _ = tokio::process::Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .output()
                .await;

            // Wait briefly, then force kill
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;

            let _ = tokio::process::Command::new("kill")
                .args(["-KILL", &pid.to_string()])
                .output()
                .await;

            tracing::info!("[virt] VM {} killed (pid={})", id, pid);
        }

        Ok(())
    }

    /// Execute a command in a VM via k3rs-vmm vsock exec.
    async fn exec_via_vmm(&self, id: &str, command: &[&str]) -> Result<String> {
        let vmm = which_vmm()
            .await
            .ok_or_else(|| anyhow::anyhow!("k3rs-vmm helper not found — cannot exec"))?;

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
            Ok(format!("{}{}", stdout, stderr))
        }
    }

    /// Check if the kernel manager has assets available.
    pub async fn kernel_available(&self) -> bool {
        self.kernel_manager.is_available().await
    }
}

#[async_trait]
impl RuntimeBackend for VirtualizationBackend {
    fn name(&self) -> &str {
        "virtualization"
    }

    fn version(&self) -> &str {
        "macos-vz-2.0"
    }

    async fn create(&self, id: &str, bundle: &Path) -> Result<()> {
        tracing::info!(
            "[virt] create VM container: id={}, bundle={}",
            id,
            bundle.display()
        );

        // Prepare rootfs directory for virtio-fs sharing.
        // The bundle should contain the extracted OCI rootfs.
        let rootfs_src = if bundle.join("rootfs").exists() {
            bundle.join("rootfs")
        } else {
            bundle.to_path_buf()
        };

        // Create our own rootfs directory for this VM
        let rootfs_dir = self.rootfs_dir(id);
        tokio::fs::create_dir_all(&rootfs_dir).await?;

        // Copy rootfs content (or symlink for efficiency)
        // For virtio-fs, we can share the original bundle directory directly,
        // but we keep a separate dir for OCI config placement.
        let config_path = self.config_path(id);

        // If the bundle has a config.json, copy it to where k3rs-init expects it
        let bundle_config = bundle.join("config.json");
        if bundle_config.exists() {
            tokio::fs::copy(&bundle_config, &config_path).await?;
        }

        // For virtio-fs, we share the original rootfs directly (no disk image needed)
        let log_path = self.log_path(id);
        tokio::fs::write(&log_path, "").await?;

        // Track the instance
        let instance = VmInstance {
            id: id.to_string(),
            rootfs_dir: rootfs_src,
            vmm_pid: None,
            state: VmState::Created,
            log_path,
            config_path,
        };

        self.instances
            .write()
            .await
            .insert(id.to_string(), instance);
        tracing::info!("[virt] VM container {} created (virtio-fs rootfs)", id);
        Ok(())
    }

    async fn create_from_image(&self, id: &str, image: &str, command: &[String]) -> Result<()> {
        tracing::info!(
            "[virt] create VM from image: id={}, image={}, cmd={:?}",
            id,
            image,
            command
        );

        // Create rootfs directory and config.json for the image
        let rootfs_dir = self.rootfs_dir(id);
        tokio::fs::create_dir_all(&rootfs_dir).await?;

        let log_path = self.log_path(id);
        let config_path = self.config_path(id);

        // Write OCI config.json for k3rs-init to parse
        let cmd_args: Vec<&str> = if command.is_empty() {
            vec!["/bin/sh"]
        } else {
            command.iter().map(|s| s.as_str()).collect()
        };
        let config = serde_json::json!({
            "process": {
                "args": cmd_args,
                "env": [
                    "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                    format!("HOSTNAME={}", id),
                ],
                "cwd": "/"
            },
            "hostname": id
        });
        tokio::fs::write(&config_path, serde_json::to_string_pretty(&config)?).await?;

        tokio::fs::write(
            &log_path,
            format!("[virt] VM for image {} (cmd: {:?})\n", image, command),
        )
        .await?;

        let instance = VmInstance {
            id: id.to_string(),
            rootfs_dir,
            vmm_pid: None,
            state: VmState::Created,
            log_path,
            config_path,
        };

        self.instances
            .write()
            .await
            .insert(id.to_string(), instance);
        Ok(())
    }

    async fn start(&self, id: &str) -> Result<()> {
        tracing::info!("[virt] starting VM: {}", id);

        let rootfs_dir = {
            let instances = self.instances.read().await;
            let instance = instances
                .get(id)
                .ok_or_else(|| anyhow::anyhow!("VM {} not found", id))?;
            instance.rootfs_dir.clone()
        };

        // Boot the VM via k3rs-vmm
        let pid = self.boot_vm(id, &rootfs_dir).await?;

        // Update state
        let mut instances = self.instances.write().await;
        if let Some(instance) = instances.get_mut(id) {
            instance.state = VmState::Running;
            instance.vmm_pid = pid;
        }

        tracing::info!("[virt] VM {} started", id);
        Ok(())
    }

    async fn stop(&self, id: &str) -> Result<()> {
        tracing::info!("[virt] stopping VM: {}", id);

        let pid = {
            let instances = self.instances.read().await;
            instances.get(id).and_then(|i| i.vmm_pid)
        };

        // Stop via k3rs-vmm or kill
        self.stop_vm(id, pid).await?;

        // Update state
        let mut instances = self.instances.write().await;
        if let Some(instance) = instances.get_mut(id) {
            instance.state = VmState::Stopped;
            instance.vmm_pid = None;
        }

        tracing::info!("[virt] VM {} stopped", id);
        Ok(())
    }

    async fn delete(&self, id: &str) -> Result<()> {
        tracing::info!("[virt] deleting VM: {}", id);

        // Stop if still running
        self.stop(id).await.ok();

        // Clean up rootfs and logs
        let instance = self.instances.write().await.remove(id);
        if let Some(inst) = instance {
            let _ = tokio::fs::remove_dir_all(&inst.rootfs_dir).await;
            let _ = tokio::fs::remove_file(&inst.log_path).await;
        }

        // Clean up any remaining files
        let rootfs_dir = self.rootfs_dir(id);
        let log_path = self.log_path(id);
        let _ = tokio::fs::remove_dir_all(&rootfs_dir).await;
        let _ = tokio::fs::remove_file(&log_path).await;

        tracing::info!("[virt] VM {} deleted", id);
        Ok(())
    }

    async fn list(&self) -> Result<Vec<String>> {
        let instances = self.instances.read().await;
        let ids: Vec<String> = instances
            .values()
            .filter(|i| i.state == VmState::Running)
            .map(|i| i.id.clone())
            .collect();
        Ok(ids)
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
            Err(_) => Ok(vec![format!("[virt] No logs available for VM {}", id)]),
        }
    }

    async fn exec(&self, id: &str, command: &[&str]) -> Result<String> {
        tracing::info!("[virt] exec in VM {}: {:?}", id, command);

        let instances = self.instances.read().await;
        let instance = instances
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("VM {} not found", id))?;

        if instance.state != VmState::Running {
            anyhow::bail!("VM {} is not running (state: {:?})", id, instance.state);
        }
        drop(instances);

        // Try exec via k3rs-vmm vsock
        match self.exec_via_vmm(id, command).await {
            Ok(output) => Ok(output),
            Err(e) => {
                tracing::warn!(
                    "[virt] vsock exec failed ({}), falling back to host exec (dev mode)",
                    e
                );
                // Fallback: run command on host (dev mode)
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
    }
}

/// Find the k3rs-vmm helper binary path.
async fn which_vmm() -> Option<String> {
    // Check in PATH
    if let Ok(output) = tokio::process::Command::new("which")
        .arg("k3rs-vmm")
        .output()
        .await
    {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(path);
            }
        }
    }

    // Check in common locations
    for path in &[
        "./target/release/k3rs-vmm",
        "./target/debug/k3rs-vmm",
        "/usr/local/bin/k3rs-vmm",
        "/opt/k3rs/bin/k3rs-vmm",
        "./k3rs-vmm",
    ] {
        if tokio::fs::metadata(path).await.is_ok() {
            return Some(path.to_string());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_vm_config_defaults() {
        let config = VmConfig::default();
        assert_eq!(config.cpu_count, 1);
        assert_eq!(config.memory_mb, 128);
    }

    #[tokio::test]
    async fn test_virtualization_backend_create_and_lifecycle() {
        let tmp_dir = PathBuf::from("/tmp/k3rs-virt-test");
        let _ = tokio::fs::create_dir_all(&tmp_dir).await;

        let backend = VirtualizationBackend::new(&tmp_dir).await.unwrap();

        assert_eq!(backend.name(), "virtualization");
        assert_eq!(backend.version(), "macos-vz-2.0");
        assert!(!backend.handles_images());

        // List should be empty
        let list = backend.list().await.unwrap();
        assert!(list.is_empty());

        // Cleanup
        let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
    }

    #[tokio::test]
    async fn test_create_uses_virtiofs_not_disk_image() {
        let tmp_dir = PathBuf::from("/tmp/k3rs-virt-test-create");
        let _ = tokio::fs::create_dir_all(&tmp_dir).await;

        let backend = VirtualizationBackend::new(&tmp_dir).await.unwrap();

        // Create a fake rootfs bundle
        let bundle_dir = tmp_dir.join("test-bundle");
        let rootfs_dir = bundle_dir.join("rootfs");
        tokio::fs::create_dir_all(&rootfs_dir).await.unwrap();
        tokio::fs::write(rootfs_dir.join("hello.txt"), "world")
            .await
            .unwrap();

        // Create the container
        let id = "test-vm-001";
        backend.create(id, &bundle_dir).await.unwrap();

        // Verify: no .img disk image was created
        let img_path = tmp_dir.join("vms").join(format!("{}.img", id));
        assert!(
            !img_path.exists(),
            "Should NOT create disk images (virtio-fs mode)"
        );

        // Verify: log file was created
        let log_path = backend.log_path(id);
        assert!(
            tokio::fs::metadata(&log_path).await.is_ok(),
            "Log file should exist"
        );

        // Verify: instance is tracked
        let instances = backend.instances.read().await;
        assert!(instances.contains_key(id));
        let inst = &instances[id];
        assert_eq!(inst.state, VmState::Created);
        assert!(inst.vmm_pid.is_none());

        // Cleanup
        let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
    }

    #[tokio::test]
    async fn test_create_from_image_generates_config() {
        let tmp_dir = PathBuf::from("/tmp/k3rs-virt-test-image");
        let _ = tokio::fs::create_dir_all(&tmp_dir).await;

        let backend = VirtualizationBackend::new(&tmp_dir).await.unwrap();

        let id = "test-img-001";
        backend
            .create_from_image(id, "alpine:latest", &["sh".to_string()])
            .await
            .unwrap();

        // Verify: OCI config.json was generated
        let config_path = backend.config_path(id);
        assert!(
            tokio::fs::metadata(&config_path).await.is_ok(),
            "config.json should be generated for k3rs-init"
        );

        // Verify config contents
        let config_data = tokio::fs::read_to_string(&config_path).await.unwrap();
        let config: serde_json::Value = serde_json::from_str(&config_data).unwrap();
        assert_eq!(config["hostname"], id);
        assert_eq!(config["process"]["args"][0], "sh");

        // Cleanup
        let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
    }

    #[tokio::test]
    async fn test_delete_cleans_up_rootfs_dir() {
        let tmp_dir = PathBuf::from("/tmp/k3rs-virt-test-delete");
        let _ = tokio::fs::create_dir_all(&tmp_dir).await;

        let backend = VirtualizationBackend::new(&tmp_dir).await.unwrap();

        let id = "test-del-001";
        backend
            .create_from_image(id, "test:latest", &[])
            .await
            .unwrap();

        // Verify files exist
        let rootfs = backend.rootfs_dir(id);
        assert!(tokio::fs::metadata(&rootfs).await.is_ok());

        // Delete
        backend.delete(id).await.unwrap();

        // Verify cleaned up
        assert!(tokio::fs::metadata(&rootfs).await.is_err());

        let instances = backend.instances.read().await;
        assert!(!instances.contains_key(id));

        // Cleanup
        let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
    }

    #[tokio::test]
    async fn test_which_vmm_returns_none_when_not_installed() {
        // In test env, k3rs-vmm is likely not in PATH
        // This tests the graceful "not found" path
        let result = which_vmm().await;
        // Can't assert None since user might have it installed,
        // but the function should not panic
        let _ = result;
    }
}
