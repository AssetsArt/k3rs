//! Kernel & initrd asset management for microVM backends.
//!
//! Downloads and caches a minimal Linux kernel + initrd required to boot
//! lightweight VMs via Virtualization.framework (macOS) or Firecracker (Linux).

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// Default kernel/initrd location
const KERNEL_DIR: &str = "/var/lib/k3rs";
const KERNEL_FILENAME: &str = "vmlinux";
const INITRD_FILENAME: &str = "initrd.img";

/// GitHub release base URL for pre-built kernels.
/// These are minimal 6.x kernels with virtio drivers + virtiofs support.
const KERNEL_RELEASE_URL: &str = "https://github.com/k3rs-project/kernel/releases/latest/download";

/// Manages kernel + initrd assets for microVM backends.
pub struct KernelManager {
    /// Directory where kernel assets are stored
    kernel_dir: PathBuf,
}

impl KernelManager {
    /// Create a KernelManager with the default directory (`/var/lib/k3rs`).
    pub fn new() -> Self {
        Self {
            kernel_dir: PathBuf::from(KERNEL_DIR),
        }
    }

    /// Create a KernelManager with a custom directory.
    pub fn with_dir(dir: &Path) -> Self {
        Self {
            kernel_dir: dir.to_path_buf(),
        }
    }

    /// Path to the kernel binary.
    pub fn kernel_path(&self) -> PathBuf {
        self.kernel_dir.join(KERNEL_FILENAME)
    }

    /// Path to the initrd image.
    pub fn initrd_path(&self) -> PathBuf {
        self.kernel_dir.join(INITRD_FILENAME)
    }

    /// Ensure kernel and initrd are available.
    ///
    /// Returns `(kernel_path, initrd_path)`.
    /// If assets are missing, attempts to download them.
    pub async fn ensure_available(&self) -> Result<(PathBuf, Option<PathBuf>)> {
        tokio::fs::create_dir_all(&self.kernel_dir)
            .await
            .context("Failed to create kernel directory")?;

        let kernel = self.kernel_path();
        let initrd = self.initrd_path();

        // Check if kernel exists
        if !tokio::fs::metadata(&kernel).await.is_ok() {
            info!(
                "Kernel not found at {} — attempting download",
                kernel.display()
            );
            match self.download_kernel().await {
                Ok(_) => info!("Kernel downloaded successfully"),
                Err(e) => {
                    warn!(
                        "Failed to download kernel: {}. VM boot will require manual kernel placement.",
                        e
                    );
                    return Ok((kernel, None));
                }
            }
        } else {
            info!("Using cached kernel at {}", kernel.display());
        }

        let initrd_result = if tokio::fs::metadata(&initrd).await.is_ok() {
            Some(initrd)
        } else {
            // Try to download initrd
            match self.download_initrd().await {
                Ok(_) => Some(self.initrd_path()),
                Err(e) => {
                    warn!("Initrd not available ({}), will boot without initrd", e);
                    None
                }
            }
        };

        Ok((kernel, initrd_result))
    }

    /// Check if a kernel is available (without downloading).
    pub async fn is_available(&self) -> bool {
        tokio::fs::metadata(self.kernel_path()).await.is_ok()
    }

    /// Download the kernel binary.
    async fn download_kernel(&self) -> Result<()> {
        let arch = match std::env::consts::ARCH {
            "aarch64" => "arm64",
            "x86_64" => "amd64",
            other => other,
        };

        let url = format!("{}/vmlinux-{}", KERNEL_RELEASE_URL, arch);
        let dest = self.kernel_path();

        info!("Downloading kernel from {}", url);
        self.download_file(&url, &dest).await?;

        info!(
            "Kernel saved to {} ({})",
            dest.display(),
            format_file_size(&dest).await
        );
        Ok(())
    }

    /// Download the initrd image.
    async fn download_initrd(&self) -> Result<()> {
        let arch = match std::env::consts::ARCH {
            "aarch64" => "arm64",
            "x86_64" => "amd64",
            other => other,
        };

        let url = format!("{}/initrd-{}.img", KERNEL_RELEASE_URL, arch);
        let dest = self.initrd_path();

        info!("Downloading initrd from {}", url);
        self.download_file(&url, &dest).await?;

        info!(
            "Initrd saved to {} ({})",
            dest.display(),
            format_file_size(&dest).await
        );
        Ok(())
    }

    /// Download a file from a URL to a local path.
    async fn download_file(&self, url: &str, dest: &Path) -> Result<()> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(10))
            .build()?;

        let response = client
            .get(url)
            .send()
            .await
            .context("HTTP request failed")?;

        if !response.status().is_success() {
            anyhow::bail!("Download failed: HTTP {} for {}", response.status(), url);
        }

        let bytes = response.bytes().await?;
        tokio::fs::write(dest, &bytes).await?;

        // Make kernel executable (in case it needs to be)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755))?;
        }

        // Save version metadata
        self.save_version_info(url).await?;

        Ok(())
    }

    /// Save download metadata for tracking.
    async fn save_version_info(&self, source_url: &str) -> Result<()> {
        let info_path = self.kernel_dir.join("kernel-info.json");

        let info = serde_json::json!({
            "source": source_url,
            "downloaded_at": chrono::Utc::now().to_rfc3339(),
            "arch": std::env::consts::ARCH,
            "os": std::env::consts::OS,
        });

        tokio::fs::write(&info_path, serde_json::to_string_pretty(&info)?).await?;
        Ok(())
    }
}

/// Human-readable file size.
async fn format_file_size(path: &Path) -> String {
    match tokio::fs::metadata(path).await {
        Ok(meta) => {
            let bytes = meta.len();
            if bytes >= 1_000_000 {
                format!("{:.1} MB", bytes as f64 / 1_048_576.0)
            } else if bytes >= 1_000 {
                format!("{:.0} KB", bytes as f64 / 1_024.0)
            } else {
                format!("{} B", bytes)
            }
        }
        Err(_) => "unknown".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kernel_manager_paths() {
        let km = KernelManager::new();
        assert_eq!(km.kernel_path(), PathBuf::from("/var/lib/k3rs/vmlinux"));
        assert_eq!(km.initrd_path(), PathBuf::from("/var/lib/k3rs/initrd.img"));
    }

    #[test]
    fn test_kernel_manager_custom_dir() {
        let km = KernelManager::with_dir(Path::new("/tmp/test-kernels"));
        assert_eq!(km.kernel_path(), PathBuf::from("/tmp/test-kernels/vmlinux"));
        assert_eq!(
            km.initrd_path(),
            PathBuf::from("/tmp/test-kernels/initrd.img")
        );
    }
}
