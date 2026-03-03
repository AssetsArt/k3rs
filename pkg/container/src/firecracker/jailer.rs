//! Firecracker Jailer wrapper for chroot + seccomp + cgroups isolation.
//!
//! The Jailer wraps the Firecracker process in a chroot jail with:
//! - `unshare(CLONE_NEWPID)` — PID namespace isolation
//! - seccomp filtering — syscall allowlisting
//! - cgroup resource limits — CPU/memory/IO
//! - UID/GID mapping — drops privileges
//! - `--daemonize` — Jailer parent exits, Firecracker becomes orphan

use anyhow::Result;
use std::path::{Path, PathBuf};
use tracing::info;

/// Wraps Firecracker with the Jailer for enhanced isolation.
pub struct Jailer {
    jailer_bin: PathBuf,
    chroot_base: PathBuf,
}

impl Jailer {
    pub fn new(jailer_bin: &Path) -> Self {
        Self {
            jailer_bin: jailer_bin.to_path_buf(),
            chroot_base: PathBuf::from("/tmp/k3rs-jailer"),
        }
    }

    /// Build a Command that wraps Firecracker with the Jailer.
    ///
    /// The Jailer creates a chroot at:
    ///   `{chroot_base}/firecracker/{id}/root/`
    ///
    /// Inside the chroot, Firecracker runs with reduced privileges
    /// and a seccomp filter that only allows necessary syscalls.
    pub fn wrap_command(
        &self,
        id: &str,
        firecracker_bin: &Path,
        api_socket_name: &str,
    ) -> std::process::Command {
        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };

        // If running as root, use UID/GID 65534 (nobody) for the jailed process
        let (exec_uid, exec_gid) = if uid == 0 {
            (65534u32, 65534u32)
        } else {
            (uid, gid)
        };

        let mut cmd = std::process::Command::new(&self.jailer_bin);
        cmd.args([
            "--id",
            id,
            "--exec-file",
            &firecracker_bin.to_string_lossy(),
            "--uid",
            &exec_uid.to_string(),
            "--gid",
            &exec_gid.to_string(),
            "--chroot-base-dir",
            &self.chroot_base.to_string_lossy(),
            "--daemonize",
            "--",
            "--api-sock",
            api_socket_name,
        ]);

        cmd
    }

    /// Get the chroot directory for a jailed VM.
    ///
    /// Path: `{chroot_base}/firecracker/{id}/root/`
    pub fn chroot_dir(&self, id: &str) -> PathBuf {
        self.chroot_base.join("firecracker").join(id).join("root")
    }

    /// Get the API socket path inside the jailer chroot.
    ///
    /// When using the Jailer, the API socket is relative to the chroot.
    pub fn api_socket_in_chroot(&self, id: &str, socket_name: &str) -> PathBuf {
        self.chroot_dir(id).join(socket_name)
    }

    /// Prepare the jailer chroot with required files.
    ///
    /// The Jailer expects the Firecracker binary and kernel/rootfs
    /// to be accessible inside the chroot.
    pub async fn prepare_chroot(
        &self,
        id: &str,
        kernel_path: &Path,
        rootfs_path: &Path,
    ) -> Result<()> {
        let chroot = self.chroot_dir(id);
        tokio::fs::create_dir_all(&chroot).await?;

        // Hard-link or copy kernel into chroot
        let kernel_dest = chroot.join("vmlinux");
        if tokio::fs::hard_link(kernel_path, &kernel_dest)
            .await
            .is_err()
        {
            tokio::fs::copy(kernel_path, &kernel_dest).await?;
        }

        // Hard-link or copy rootfs image into chroot
        let rootfs_name = rootfs_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let rootfs_dest = chroot.join(&rootfs_name);
        if tokio::fs::hard_link(rootfs_path, &rootfs_dest)
            .await
            .is_err()
        {
            tokio::fs::copy(rootfs_path, &rootfs_dest).await?;
        }

        info!(
            "[fc-jailer] Prepared chroot for {} at {}",
            id,
            chroot.display()
        );

        Ok(())
    }

    /// Cleanup the jailer chroot after VM deletion.
    pub async fn cleanup(&self, id: &str) {
        let dir = self.chroot_base.join("firecracker").join(id);
        if dir.exists() {
            let _ = tokio::fs::remove_dir_all(&dir).await;
            info!("[fc-jailer] Cleaned up chroot for {}", id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_chroot_dir() {
        let jailer = Jailer::new(Path::new("/usr/local/bin/jailer"));
        assert_eq!(
            jailer.chroot_dir("test-vm"),
            PathBuf::from("/tmp/k3rs-jailer/firecracker/test-vm/root")
        );
    }

    #[test]
    fn test_api_socket_in_chroot() {
        let jailer = Jailer::new(Path::new("/usr/local/bin/jailer"));
        assert_eq!(
            jailer.api_socket_in_chroot("test-vm", "api.sock"),
            PathBuf::from("/tmp/k3rs-jailer/firecracker/test-vm/root/api.sock")
        );
    }
}
