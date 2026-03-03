//! Dual rootfs handler: ext4 block device + virtiofsd shared directory.
//!
//! Firecracker supports two rootfs strategies:
//! - **ext4 block device**: Uses `mkfs.ext4 -d` to populate directly (no root needed).
//! - **virtiofsd**: Shared directory via vhost-user-fs (Firecracker v1.5+).
//!
//! This module auto-detects virtiofsd availability and falls back to ext4.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tracing::info;

/// Rootfs mode for a Firecracker VM.
#[derive(Debug, Clone)]
pub enum FcRootfsMode {
    /// ext4 block device image (mkfs.ext4 -d, no root required)
    Ext4 { image_path: PathBuf },
    /// virtiofsd shared directory (no disk image)
    Virtiofsd {
        shared_dir: PathBuf,
        socket_path: PathBuf,
        /// PID of the virtiofsd daemon process
        virtiofsd_pid: Option<u32>,
    },
}

/// Manages rootfs creation for Firecracker VMs.
pub struct FcRootfsManager;

impl FcRootfsManager {
    /// Detect the best rootfs mode for this system.
    pub fn detect_mode() -> RootfsStrategy {
        if Self::virtiofsd_available() {
            RootfsStrategy::Virtiofsd
        } else {
            RootfsStrategy::Ext4
        }
    }

    /// Check if virtiofsd is available in PATH.
    fn virtiofsd_available() -> bool {
        std::process::Command::new("which")
            .arg("virtiofsd")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Create an ext4 rootfs image from an OCI rootfs directory.
    ///
    /// Uses `mkfs.ext4 -d` to populate the filesystem directly from the source
    /// directory at format time, avoiding loop mounts that require root.
    pub async fn create_ext4_image(rootfs_dir: &Path, img_path: &Path) -> Result<PathBuf> {
        let dir_size = Self::dir_size(rootfs_dir).await?;
        let img_size_mb = std::cmp::max((dir_size / 1_048_576) + 64, 128); // min 128MB

        info!(
            "[fc-rootfs] Creating ext4 image: {} ({}MB, from {} source)",
            img_path.display(),
            img_size_mb,
            rootfs_dir.display()
        );

        // Create sparse file
        let output = tokio::process::Command::new("truncate")
            .args([
                "-s",
                &format!("{}M", img_size_mb),
                &img_path.to_string_lossy(),
            ])
            .output()
            .await
            .context("truncate failed")?;
        if !output.status.success() {
            anyhow::bail!(
                "truncate failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        // Format as ext4 and populate from rootfs directory in one step.
        // The -d flag copies the directory contents into the image at format time,
        // avoiding mount -o loop which requires root privileges.
        let output = tokio::process::Command::new("mkfs.ext4")
            .args([
                "-F",
                "-q",
                "-d",
                &rootfs_dir.to_string_lossy(),
                &img_path.to_string_lossy(),
            ])
            .output()
            .await
            .context("mkfs.ext4 failed")?;
        if !output.status.success() {
            anyhow::bail!(
                "mkfs.ext4 -d failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        info!(
            "[fc-rootfs] ext4 image created: {} ({}MB)",
            img_path.display(),
            img_size_mb
        );
        Ok(img_path.to_path_buf())
    }

    /// Start virtiofsd daemon for a shared directory.
    ///
    /// Returns the virtiofsd PID and socket path.
    pub async fn start_virtiofsd(rootfs_dir: &Path, socket_path: &Path) -> Result<u32> {
        info!(
            "[fc-rootfs] Starting virtiofsd: shared={} socket={}",
            rootfs_dir.display(),
            socket_path.display()
        );

        // Remove stale socket if exists
        let _ = tokio::fs::remove_file(socket_path).await;

        let mut cmd = std::process::Command::new("virtiofsd");
        cmd.args([
            "--socket-path",
            &socket_path.to_string_lossy(),
            "--shared-dir",
            &rootfs_dir.to_string_lossy(),
            "--cache",
            "auto",
        ]);

        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());

        // Detach from agent session
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

        let child = cmd.spawn().context("Failed to spawn virtiofsd")?;
        let pid = child.id();

        // Wait for socket to appear
        for _ in 0..50 {
            if socket_path.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        if !socket_path.exists() {
            anyhow::bail!(
                "virtiofsd socket did not appear at {}",
                socket_path.display()
            );
        }

        info!(
            "[fc-rootfs] virtiofsd started (pid={}, socket={})",
            pid,
            socket_path.display()
        );
        Ok(pid)
    }

    /// Stop a virtiofsd daemon by PID.
    pub async fn stop_virtiofsd(pid: u32) {
        let _ = tokio::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .output()
            .await;
    }

    /// Calculate total size of a directory tree in bytes.
    async fn dir_size(path: &Path) -> Result<u64> {
        let output = tokio::process::Command::new("du")
            .args(["-sb", &path.to_string_lossy()])
            .output()
            .await?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let size: u64 = stdout
            .split_whitespace()
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        Ok(size)
    }
}

/// Strategy for rootfs creation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RootfsStrategy {
    Ext4,
    Virtiofsd,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_mode_returns_strategy() {
        let mode = FcRootfsManager::detect_mode();
        // Just verify it doesn't panic and returns a valid strategy
        assert!(mode == RootfsStrategy::Ext4 || mode == RootfsStrategy::Virtiofsd);
    }
}
