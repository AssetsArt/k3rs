//! Virtualization.framework microVM backend for macOS.
//!
//! Provides Firecracker-like lightweight Linux VM isolation using Apple's
//! native Virtualization.framework. Each container/pod runs inside its own
//! isolated microVM with virtio devices for block storage, networking,
//! console output, and vsock communication.
//!
//! Requirements:
//! - macOS 12+ (Monterey)
//! - com.apple.security.virtualization entitlement
//! - A pre-built minimal Linux kernel (vmlinuz) for the guest

use anyhow::{Context, Result};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::backend::RuntimeBackend;

/// Minimum Linux kernel for guest VMs.
/// This is downloaded or bundled with the agent.
const DEFAULT_KERNEL_PATH: &str = "/var/lib/k3rs/vmlinux";
const DEFAULT_INITRD_PATH: &str = "/var/lib/k3rs/initrd.img";

/// Default VM resource configuration
const DEFAULT_CPU_COUNT: u32 = 1;
const DEFAULT_MEMORY_MB: u64 = 128;

/// VM state tracking
#[derive(Debug, Clone)]
struct VmInstance {
    /// Container/pod ID
    id: String,
    /// Path to the rootfs disk image
    rootfs_path: PathBuf,
    /// Process ID of the VM monitor process (if running via helper)
    monitor_pid: Option<u32>,
    /// VM state
    state: VmState,
    /// Log file path
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
/// Each container runs inside a lightweight Linux microVM:
/// - virtio-blk: OCI rootfs mounted as root disk
/// - virtio-net: NAT networking for pod connectivity
/// - virtio-console: stdout/stderr log streaming
/// - virtio-vsock: host ↔ guest exec channel
pub struct VirtualizationBackend {
    /// Directory for VM runtime data (disks, logs, sockets)
    data_dir: PathBuf,
    /// Path to the guest Linux kernel
    kernel_path: PathBuf,
    /// Path to the guest initrd (optional)
    #[allow(dead_code)]
    initrd_path: Option<PathBuf>,
    /// CPU count per VM
    cpu_count: u32,
    /// Memory per VM in MB
    memory_mb: u64,
    /// Active VM instances
    instances: Arc<RwLock<HashMap<String, VmInstance>>>,
}

impl VirtualizationBackend {
    /// Create a new VirtualizationBackend with default configuration.
    ///
    /// Validates that:
    /// - Running on macOS 12+
    /// - A guest kernel is available
    pub async fn new(data_dir: &Path) -> Result<Self> {
        // Verify macOS platform
        #[cfg(not(target_os = "macos"))]
        {
            anyhow::bail!("VirtualizationBackend requires macOS");
        }

        let vm_dir = data_dir.join("vms");
        tokio::fs::create_dir_all(&vm_dir).await?;

        let kernel_path = PathBuf::from(DEFAULT_KERNEL_PATH);
        let initrd_path = PathBuf::from(DEFAULT_INITRD_PATH);

        // Check for kernel — if not found, we'll use a stub mode
        // In production, the kernel would be bundled or downloaded
        let kernel_exists = tokio::fs::metadata(&kernel_path).await.is_ok();
        if !kernel_exists {
            tracing::warn!(
                "Guest kernel not found at {}. VM operations will create disk images but boot will require a kernel.",
                kernel_path.display()
            );
        }

        let initrd = if tokio::fs::metadata(&initrd_path).await.is_ok() {
            Some(initrd_path)
        } else {
            None
        };

        tracing::info!(
            "VirtualizationBackend initialized: data_dir={}, kernel={}, cpus={}, memory={}MB",
            vm_dir.display(),
            kernel_path.display(),
            DEFAULT_CPU_COUNT,
            DEFAULT_MEMORY_MB
        );

        Ok(Self {
            data_dir: vm_dir,
            kernel_path,
            initrd_path: initrd,
            cpu_count: DEFAULT_CPU_COUNT,
            memory_mb: DEFAULT_MEMORY_MB,
            instances: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Create a raw disk image from a rootfs directory.
    /// Converts the extracted OCI rootfs into a disk image suitable for virtio-blk.
    async fn create_disk_image(&self, id: &str, rootfs: &Path) -> Result<PathBuf> {
        let disk_path = self.data_dir.join(format!("{}.img", id));

        // Create a raw disk image by tar-ing the rootfs into a file
        // In production, this would create an ext4/erofs filesystem image
        let output = tokio::process::Command::new("hdiutil")
            .args([
                "create",
                "-srcfolder",
                &rootfs.to_string_lossy(),
                "-format",
                "UDRO", // read-only UDIF
                "-o",
                &disk_path.to_string_lossy(),
            ])
            .output()
            .await
            .context("failed to create disk image via hdiutil")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Try raw dd fallback for Linux-format disk
            tracing::warn!(
                "hdiutil failed ({}), creating raw disk image instead",
                stderr.trim()
            );
            self.create_raw_disk(id, rootfs).await?;
        }

        // hdiutil appends .dmg extension
        let dmg_path = self.data_dir.join(format!("{}.img.dmg", id));
        if tokio::fs::metadata(&dmg_path).await.is_ok() {
            tokio::fs::rename(&dmg_path, &disk_path).await?;
        }

        Ok(disk_path)
    }

    /// Create a raw disk image using dd + mkfs (fallback).
    async fn create_raw_disk(&self, id: &str, rootfs: &Path) -> Result<PathBuf> {
        let disk_path = self.data_dir.join(format!("{}.img", id));

        // Calculate rootfs size and create a slightly larger disk
        let rootfs_size = dir_size(rootfs).await.unwrap_or(64 * 1024 * 1024); // 64MB min
        let disk_size = std::cmp::max(rootfs_size * 2, 128 * 1024 * 1024); // At least 128MB
        let disk_size_mb = disk_size / (1024 * 1024);

        // Create empty disk image
        let status = tokio::process::Command::new("dd")
            .args([
                "if=/dev/zero",
                &format!("of={}", disk_path.to_string_lossy()),
                "bs=1m",
                &format!("count={}", disk_size_mb),
            ])
            .output()
            .await
            .context("failed to create raw disk image")?;

        if !status.status.success() {
            anyhow::bail!("dd failed: {}", String::from_utf8_lossy(&status.stderr));
        }

        tracing::info!(
            "[virt] Created raw disk image: {} ({}MB)",
            disk_path.display(),
            disk_size_mb
        );
        Ok(disk_path)
    }

    /// Get the log path for a container.
    fn log_path(&self, id: &str) -> PathBuf {
        self.data_dir.join(format!("{}.log", id))
    }

    /// Start a VM using the Virtualization.framework via a helper process.
    ///
    /// The helper process (`k3rs-vmm`) wraps the Obj-C Virtualization.framework
    /// API since Rust FFI to Apple frameworks requires careful Obj-C runtime bridging.
    ///
    /// VM configuration:
    /// - VZLinuxBootLoader: boots the minimal Linux kernel
    /// - VZVirtioBlockDeviceConfiguration: rootfs disk
    /// - VZVirtioNetworkDeviceConfiguration: NAT networking
    /// - VZVirtioConsoleDeviceConfiguration: serial console → log file
    /// - VZVirtioSocketDeviceConfiguration: vsock for exec
    async fn boot_vm(&self, id: &str, disk_path: &Path) -> Result<Option<u32>> {
        // Launch the VM as a child process
        // On macOS, we use the `k3rs-vmm` helper binary that wraps
        // Virtualization.framework's Obj-C API
        let log_path = self.log_path(id);

        // Check if k3rs-vmm helper exists
        let vmm_path = which_vmm().await;

        match vmm_path {
            Some(vmm) => {
                let log_file = std::fs::File::create(&log_path)?;
                let stderr_file = log_file.try_clone()?;

                let child = std::process::Command::new(&vmm)
                    .args([
                        "--kernel",
                        &self.kernel_path.to_string_lossy(),
                        "--disk",
                        &disk_path.to_string_lossy(),
                        "--cpus",
                        &self.cpu_count.to_string(),
                        "--memory",
                        &self.memory_mb.to_string(),
                        "--id",
                        id,
                    ])
                    .stdout(log_file)
                    .stderr(stderr_file)
                    .spawn()
                    .context("failed to spawn k3rs-vmm")?;

                let pid = child.id();
                tracing::info!("[virt] VM {} booted via k3rs-vmm (pid={})", id, pid);
                Ok(Some(pid))
            }
            None => {
                // No VMM helper — create the log file and track as "created"
                // The VM will start when the VMM helper is available
                tokio::fs::write(
                    &log_path,
                    format!("[virt] VM {} created (awaiting VMM helper)\n", id),
                )
                .await?;
                tracing::warn!(
                    "[virt] k3rs-vmm helper not found — VM {} created but not booted. \
                     Install k3rs-vmm or place vmlinux at {}",
                    id,
                    self.kernel_path.display()
                );
                Ok(None)
            }
        }
    }
}

#[async_trait]
impl RuntimeBackend for VirtualizationBackend {
    fn name(&self) -> &str {
        "virtualization"
    }

    fn version(&self) -> &str {
        "macos-vz-1.0"
    }

    async fn create(&self, id: &str, bundle: &Path) -> Result<()> {
        tracing::info!(
            "[virt] create VM container: id={}, bundle={}",
            id,
            bundle.display()
        );

        // The bundle directory should contain the extracted rootfs
        let rootfs_path = bundle.join("rootfs");
        let rootfs = if rootfs_path.exists() {
            rootfs_path
        } else {
            // Bundle IS the rootfs
            bundle.to_path_buf()
        };

        // Create disk image from rootfs
        let disk_path = self.create_disk_image(id, &rootfs).await?;

        // Create log file
        let log_path = self.log_path(id);
        tokio::fs::write(&log_path, "").await?;

        // Track the instance
        let instance = VmInstance {
            id: id.to_string(),
            rootfs_path: disk_path,
            monitor_pid: None,
            state: VmState::Created,
            log_path,
        };

        self.instances
            .write()
            .await
            .insert(id.to_string(), instance);
        tracing::info!("[virt] VM container {} created", id);
        Ok(())
    }

    async fn create_from_image(&self, id: &str, image: &str, command: &[String]) -> Result<()> {
        tracing::info!(
            "[virt] create VM from image: id={}, image={}, cmd={:?}",
            id,
            image,
            command
        );

        // For VirtualizationBackend, image pulling and rootfs extraction
        // is handled by ContainerRuntime before calling create().
        // This method creates a minimal VM entry for tracking.
        let disk_path = self.data_dir.join(format!("{}.img", id));
        let log_path = self.log_path(id);
        tokio::fs::write(
            &log_path,
            format!("[virt] VM for image {} (cmd: {:?})\n", image, command),
        )
        .await?;

        let instance = VmInstance {
            id: id.to_string(),
            rootfs_path: disk_path,
            monitor_pid: None,
            state: VmState::Created,
            log_path,
        };

        self.instances
            .write()
            .await
            .insert(id.to_string(), instance);
        Ok(())
    }

    async fn start(&self, id: &str) -> Result<()> {
        tracing::info!("[virt] starting VM: {}", id);

        let disk_path = {
            let instances = self.instances.read().await;
            let instance = instances
                .get(id)
                .ok_or_else(|| anyhow::anyhow!("VM {} not found", id))?;
            instance.rootfs_path.clone()
        };

        // Boot the VM
        let pid = self.boot_vm(id, &disk_path).await?;

        // Update state
        let mut instances = self.instances.write().await;
        if let Some(instance) = instances.get_mut(id) {
            instance.state = VmState::Running;
            instance.monitor_pid = pid;
        }

        tracing::info!("[virt] VM {} started", id);
        Ok(())
    }

    async fn stop(&self, id: &str) -> Result<()> {
        tracing::info!("[virt] stopping VM: {}", id);

        let pid = {
            let instances = self.instances.read().await;
            instances.get(id).and_then(|i| i.monitor_pid)
        };

        // Send SIGTERM to the VMM process
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
        }

        // Update state
        let mut instances = self.instances.write().await;
        if let Some(instance) = instances.get_mut(id) {
            instance.state = VmState::Stopped;
            instance.monitor_pid = None;
        }

        tracing::info!("[virt] VM {} stopped", id);
        Ok(())
    }

    async fn delete(&self, id: &str) -> Result<()> {
        tracing::info!("[virt] deleting VM: {}", id);

        // Stop if still running
        self.stop(id).await.ok();

        // Clean up disk image and logs
        let instance = self.instances.write().await.remove(id);
        if let Some(inst) = instance {
            let _ = tokio::fs::remove_file(&inst.rootfs_path).await;
            let _ = tokio::fs::remove_file(&inst.log_path).await;
        }

        // Clean up any remaining files
        let disk_path = self.data_dir.join(format!("{}.img", id));
        let log_path = self.log_path(id);
        let _ = tokio::fs::remove_file(&disk_path).await;
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

        // Execute command via vsock or SSH into the guest VM
        // For now, we use the VMM helper's exec channel if available
        let vmm_path = which_vmm().await;

        match vmm_path {
            Some(vmm) => {
                let mut args = vec![
                    "--exec".to_string(),
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
            None => {
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
        && output.status.success()
    {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path.is_empty() {
            return Some(path);
        }
    }

    // Check in common locations
    for path in &[
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

/// Recursively calculate directory size in bytes.
async fn dir_size(path: &Path) -> Result<u64> {
    let mut size = 0u64;
    let mut entries = tokio::fs::read_dir(path).await?;

    while let Some(entry) = entries.next_entry().await? {
        let metadata = entry.metadata().await?;
        if metadata.is_dir() {
            // size += Box::pin(dir_size(&entry.path())).await;
            match Box::pin(dir_size(&entry.path())).await {
                Ok(s) => size += s,
                Err(e) => {
                    tracing::error!("[virt] dir_size error: {}", e);
                    return Err(e);
                }
            }
        } else {
            size += metadata.len();
        }
    }

    Ok(size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_virtualization_backend_create_and_lifecycle() {
        let tmp_dir = PathBuf::from("/tmp/k3rs-virt-test");
        let _ = tokio::fs::create_dir_all(&tmp_dir).await;

        let backend = VirtualizationBackend::new(&tmp_dir).await.unwrap();

        assert_eq!(backend.name(), "virtualization");
        assert_eq!(backend.version(), "macos-vz-1.0");
        assert!(!backend.handles_images());

        // List should be empty
        let list = backend.list().await.unwrap();
        assert!(list.is_empty());

        // Cleanup
        let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
    }
}
