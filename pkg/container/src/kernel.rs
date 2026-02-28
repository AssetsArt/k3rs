//! Kernel & initrd asset management for microVM backends.
//!
//! Manages the minimal Linux kernel + initrd required to boot
//! lightweight VMs via Virtualization.framework (macOS) or Firecracker (Linux).
//!
//! ## Quick Setup
//!
//! ```bash
//! ./scripts/build-kernel.sh   # Builds kernel + initrd (uses Docker on macOS)
//! ```
//!
//! ## Manual Setup
//!
//! The VirtualizationBackend needs a Linux kernel compiled with virtio drivers.
//! Place these files in the kernel directory (default: `/var/lib/k3rs/`):
//!
//! ```text
//! /var/lib/k3rs/
//! ├── vmlinux       # Uncompressed Linux kernel (ELF) with virtio support
//! └── initrd.img    # Initial ramdisk containing k3rs-init as /sbin/init
//! ```
//!
//! ### Building a minimal kernel (arm64)
//! ```bash
//! # 1. Get kernel source
//! curl -LO https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-6.12.tar.xz
//! tar xf linux-6.12.tar.xz && cd linux-6.12
//!
//! # 2. Configure — minimal config with virtio drivers
//! make ARCH=arm64 defconfig
//! scripts/config -e VIRTIO -e VIRTIO_PCI -e VIRTIO_MMIO \
//!   -e VIRTIO_BLK -e VIRTIO_NET -e VIRTIO_CONSOLE \
//!   -e VIRTIOFS -e FUSE -e VIRTIO_VSOCKETS -e VSOCKETS \
//!   -e NET -e INET -e EXT4_FS -e TMPFS -e DEVTMPFS \
//!   -e DEVTMPFS_MOUNT -e PROC_FS -e SYSFS \
//!   -d MODULES -d SOUND -d DRM -d USB_SUPPORT -d WIRELESS
//! make ARCH=arm64 olddefconfig
//!
//! # 3. Build
//! make ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu- -j$(nproc) Image
//!
//! # 4. Install
//! sudo mkdir -p /var/lib/k3rs
//! sudo cp arch/arm64/boot/Image /var/lib/k3rs/vmlinux
//! ```
//!
//! ### Creating initrd with k3rs-init
//! ```bash
//! # 1. Build k3rs-init
//! cargo zigbuild --release --target aarch64-unknown-linux-musl -p k3rs-init
//!
//! # 2. Create initrd
//! mkdir -p /tmp/initrd/{sbin,dev,proc,sys,tmp,run,mnt/rootfs}
//! cp target/aarch64-unknown-linux-musl/release/k3rs-init /tmp/initrd/sbin/init
//! cd /tmp/initrd && find . | cpio -o -H newc | gzip > /var/lib/k3rs/initrd.img
//! ```

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// Default kernel/initrd location
use pkg_constants::paths::{INITRD_FILENAME, KERNEL_DIR, KERNEL_FILENAME};

/// Manages kernel + initrd assets for microVM backends.
pub struct KernelManager {
    /// Directory where kernel assets are stored
    kernel_dir: PathBuf,
    /// Optional custom URL to download kernel from (user-configurable)
    download_url: Option<String>,
}

impl KernelManager {
    /// Create a KernelManager with the default directory (`/var/lib/k3rs`).
    pub fn new() -> Self {
        Self {
            kernel_dir: PathBuf::from(KERNEL_DIR),
            download_url: None,
        }
    }

    /// Create a KernelManager with a custom directory.
    pub fn with_dir(dir: &Path) -> Self {
        Self {
            kernel_dir: dir.to_path_buf(),
            download_url: None,
        }
    }

    /// Set a custom download URL for kernel assets.
    ///
    /// The URL should point to a directory containing:
    /// - `vmlinux-arm64` / `vmlinux-amd64`
    /// - `initrd-arm64.img` / `initrd-amd64.img`
    #[allow(dead_code)]
    pub fn with_download_url(mut self, url: &str) -> Self {
        self.download_url = Some(url.to_string());
        self
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
    /// Returns `(kernel_path, Option<initrd_path>)`.
    ///
    /// If a `download_url` is configured and assets are missing, attempts download.
    /// Otherwise, just returns the paths (kernel may not exist yet).
    pub async fn ensure_available(&self) -> Result<(PathBuf, Option<PathBuf>)> {
        tokio::fs::create_dir_all(&self.kernel_dir)
            .await
            .context("Failed to create kernel directory")?;

        let kernel = self.kernel_path();
        let initrd = self.initrd_path();

        // Check if kernel exists locally
        if tokio::fs::metadata(&kernel).await.is_ok() {
            info!("Using kernel at {}", kernel.display());
        } else if let Some(ref base_url) = self.download_url {
            // Download only if a real URL is configured
            info!(
                "Kernel not found at {} — downloading from {}",
                kernel.display(),
                base_url
            );
            match self.download_kernel(base_url).await {
                Ok(_) => info!("Kernel downloaded successfully"),
                Err(e) => {
                    warn!("Failed to download kernel: {}", e);
                    self.log_setup_instructions();
                    return Ok((kernel, None));
                }
            }
        } else {
            warn!("Kernel not found at {}", kernel.display());
            self.log_setup_instructions();
            return Ok((kernel, None));
        }

        // Check initrd
        let initrd_result = if tokio::fs::metadata(&initrd).await.is_ok() {
            info!("Using initrd at {}", initrd.display());
            Some(initrd)
        } else if let Some(ref base_url) = self.download_url {
            match self.download_initrd(base_url).await {
                Ok(_) => Some(self.initrd_path()),
                Err(e) => {
                    warn!("Initrd not available ({}), will boot without initrd", e);
                    None
                }
            }
        } else {
            warn!(
                "Initrd not found at {} — VM will boot without initrd",
                initrd.display()
            );
            None
        };

        Ok((kernel, initrd_result))
    }

    /// Check if a kernel is available (without downloading).
    pub async fn is_available(&self) -> bool {
        tokio::fs::metadata(self.kernel_path()).await.is_ok()
    }

    /// Log setup instructions for the user.
    fn log_setup_instructions(&self) {
        warn!(
            "To use the VirtualizationBackend, place a Linux kernel at: {}",
            self.kernel_path().display()
        );
        warn!(
            "Run `./scripts/build-kernel.sh` to build the kernel, or see `pkg/container/src/kernel.rs` for manual instructions"
        );
    }

    /// Download the kernel binary from a configured URL.
    async fn download_kernel(&self, base_url: &str) -> Result<()> {
        let arch = match std::env::consts::ARCH {
            "aarch64" => "arm64",
            "x86_64" => "amd64",
            other => other,
        };

        let url = format!("{}/vmlinux-{}", base_url, arch);
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

    /// Download the initrd image from a configured URL.
    async fn download_initrd(&self, base_url: &str) -> Result<()> {
        let arch = match std::env::consts::ARCH {
            "aarch64" => "arm64",
            "x86_64" => "amd64",
            other => other,
        };

        let url = format!("{}/initrd-{}.img", base_url, arch);
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

    #[test]
    fn test_no_download_url_by_default() {
        let km = KernelManager::new();
        assert!(km.download_url.is_none());
    }

    #[test]
    fn test_with_download_url() {
        let km = KernelManager::new().with_download_url("https://example.com/kernels");
        assert_eq!(
            km.download_url.as_deref(),
            Some("https://example.com/kernels")
        );
    }
}
