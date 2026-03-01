//! Firecracker binary auto-download and KVM detection.
//!
//! Mirrors the pattern from `RuntimeInstaller` in `installer.rs` for OCI runtimes.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tracing::info;

use pkg_constants::runtime::FIRECRACKER_VERSION;

/// Manages Firecracker + Jailer binary discovery and auto-download.
pub struct FcInstaller;

impl FcInstaller {
    /// Check if `/dev/kvm` exists and is accessible.
    pub fn kvm_available() -> bool {
        let path = Path::new("/dev/kvm");
        if !path.exists() {
            return false;
        }
        // Try opening for read/write — KVM requires both
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .is_ok()
    }

    /// Ensure the `firecracker` binary is available. Returns its path.
    ///
    /// Search order: PATH → install_dir → auto-download from GitHub.
    pub async fn ensure_firecracker() -> Result<PathBuf> {
        // 1. Check PATH
        if let Some(path) = Self::find_in_path("firecracker") {
            info!("[fc-installer] Found firecracker in PATH: {}", path.display());
            return Ok(path);
        }

        // 2. Check install directory
        let install_dir = crate::installer::RuntimeInstaller::install_dir();
        let candidate = install_dir.join("firecracker");
        if candidate.exists() {
            info!(
                "[fc-installer] Found firecracker at {}",
                candidate.display()
            );
            return Ok(candidate);
        }

        // 3. Auto-download
        info!("[fc-installer] Firecracker not found — downloading v{}...", FIRECRACKER_VERSION);
        let (fc_path, _) = Self::download(&install_dir).await?;
        Ok(fc_path)
    }

    /// Find the `jailer` binary if available. Returns `None` if not found.
    pub async fn ensure_jailer() -> Result<Option<PathBuf>> {
        if let Some(path) = Self::find_in_path("jailer") {
            return Ok(Some(path));
        }

        let install_dir = crate::installer::RuntimeInstaller::install_dir();
        let candidate = install_dir.join("jailer");
        if candidate.exists() {
            return Ok(Some(candidate));
        }

        // Jailer is optional — don't fail if not found
        Ok(None)
    }

    /// Find a binary in PATH or standard system directories.
    fn find_in_path(name: &str) -> Option<PathBuf> {
        // Try `which` first
        if let Ok(output) = std::process::Command::new("which").arg(name).output()
            && output.status.success()
        {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(PathBuf::from(path));
            }
        }

        // Direct lookup in standard locations
        for dir in &["/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin"] {
            let candidate = PathBuf::from(dir).join(name);
            if candidate.exists() {
                return Some(candidate);
            }
        }

        None
    }

    /// Download Firecracker + Jailer from GitHub releases.
    ///
    /// URL pattern (from Containerfile.dev):
    /// ```text
    /// https://github.com/firecracker-microvm/firecracker/releases/download/
    ///   v{VERSION}/firecracker-v{VERSION}-{ARCH}.tgz
    /// ```
    ///
    /// Archive contains:
    /// ```text
    /// release-v{VERSION}-{ARCH}/firecracker-v{VERSION}-{ARCH}
    /// release-v{VERSION}-{ARCH}/jailer-v{VERSION}-{ARCH}
    /// ```
    async fn download(install_dir: &Path) -> Result<(PathBuf, PathBuf)> {
        let arch = std::env::consts::ARCH; // "x86_64" or "aarch64"
        let version = FIRECRACKER_VERSION;

        let url = format!(
            "https://github.com/firecracker-microvm/firecracker/releases/download/\
             v{version}/firecracker-v{version}-{arch}.tgz"
        );

        info!("[fc-installer] Downloading from {}", url);

        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(10))
            .build()?;

        let response = client
            .get(&url)
            .send()
            .await
            .context("Failed to download Firecracker")?;

        if !response.status().is_success() {
            anyhow::bail!(
                "Firecracker download failed: HTTP {} for {}",
                response.status(),
                url
            );
        }

        let bytes = response.bytes().await?;
        info!("[fc-installer] Downloaded {} bytes", bytes.len());

        std::fs::create_dir_all(install_dir)?;

        let fc_dest = install_dir.join("firecracker");
        let jailer_dest = install_dir.join("jailer");

        // Extract from tarball
        let fc_name_in_archive = format!("firecracker-v{}-{}", version, arch);
        let jailer_name_in_archive = format!("jailer-v{}-{}", version, arch);
        let fc_name_check = fc_name_in_archive.clone();

        tokio::task::spawn_blocking({
            let bytes = bytes.to_vec();
            let fc_dest = fc_dest.clone();
            let jailer_dest = jailer_dest.clone();
            move || -> Result<()> {
                let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(&bytes));
                let mut archive = tar::Archive::new(decoder);

                for entry in archive.entries()? {
                    let mut entry = entry?;
                    let path = entry.path()?.to_path_buf();
                    let filename = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();

                    if filename == fc_name_in_archive {
                        entry.unpack(&fc_dest)?;
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            std::fs::set_permissions(
                                &fc_dest,
                                std::fs::Permissions::from_mode(0o755),
                            )?;
                        }
                        info!("[fc-installer] Extracted firecracker to {}", fc_dest.display());
                    } else if filename == jailer_name_in_archive {
                        entry.unpack(&jailer_dest)?;
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            std::fs::set_permissions(
                                &jailer_dest,
                                std::fs::Permissions::from_mode(0o755),
                            )?;
                        }
                        info!("[fc-installer] Extracted jailer to {}", jailer_dest.display());
                    }
                }
                Ok(())
            }
        })
        .await??;

        if !fc_dest.exists() {
            anyhow::bail!(
                "Firecracker binary not found in archive (looked for {})",
                fc_name_check
            );
        }

        // Save version info
        let version_file = install_dir.join("firecracker-version.json");
        let info = serde_json::json!({
            "version": version,
            "arch": arch,
            "downloaded_at": chrono::Utc::now().to_rfc3339(),
        });
        let _ = tokio::fs::write(&version_file, serde_json::to_string_pretty(&info)?).await;

        Ok((fc_dest, jailer_dest))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_in_path_nonexistent() {
        assert!(FcInstaller::find_in_path("nonexistent-binary-k3rs-test").is_none());
    }

    #[test]
    fn test_kvm_check_returns_bool() {
        // Just verify it doesn't panic — actual result depends on host
        let _ = FcInstaller::kvm_available();
    }
}
