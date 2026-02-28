use anyhow::Result;
use pkg_constants::{paths, runtime};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// All OCI runtimes supported by the installer (re-exported for callers).
pub use runtime::SUPPORTED_RUNTIMES;

/// Default runtime when none is specified (re-exported for callers).
pub use runtime::DEFAULT_RUNTIME;

/// Manages auto-downloading OCI runtimes (youki/crun) on Linux.
pub struct RuntimeInstaller;

impl RuntimeInstaller {
    /// Ensure an OCI runtime is available. Downloads one if not found.
    ///
    /// `preferred` — which runtime to use. `None` = default (youki).
    /// Accepts: `"youki"`, `"crun"`.
    ///
    /// Search order:
    /// 1. Check preferred runtime in `$PATH`
    /// 2. Check preferred runtime in install dir
    /// 3. Auto-download preferred runtime
    /// 4. Fallback: try other supported runtimes
    pub async fn ensure_runtime(preferred: Option<&str>) -> Result<PathBuf> {
        let preferred = preferred.unwrap_or(runtime::DEFAULT_RUNTIME);

        // Build search order: preferred first, then others
        let search_order: Vec<&str> = std::iter::once(preferred)
            .chain(
                runtime::SUPPORTED_RUNTIMES
                    .iter()
                    .copied()
                    .filter(|r| *r != preferred),
            )
            .collect();

        // 1. Check $PATH
        for name in &search_order {
            if let Ok(output) = std::process::Command::new("which").arg(name).output()
                && output.status.success()
            {
                let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                info!("Found {} in PATH: {}", name, path);
                return Ok(PathBuf::from(path));
            }
        }

        // 2. Check our install directory
        let install_dir = Self::install_dir();
        for name in &search_order {
            let path = install_dir.join(name);
            if path.exists() {
                info!("Found {} at {}", name, path.display());
                return Ok(path);
            }
        }

        // 3. Auto-download preferred runtime has no prebuilt binary
        info!("No OCI runtime found — auto-downloading {}...", preferred);
        Self::download_runtime(&install_dir, preferred).await
    }

    /// Get the install directory — prefers /usr/local/bin/k3rs-runtime,
    /// falls back to $HOME/.k3rs/bin/ if no write access.
    fn install_dir() -> PathBuf {
        let system_dir = PathBuf::from(paths::RUNTIME_INSTALL_DIR);
        if std::fs::create_dir_all(&system_dir).is_ok() {
            // Test write access
            let test_file = system_dir.join(".write_test");
            if std::fs::write(&test_file, "").is_ok() {
                let _ = std::fs::remove_file(&test_file);
                return system_dir;
            }
        }

        // Fall back to home directory
        if let Ok(home) = std::env::var("HOME") {
            let home_dir = PathBuf::from(home).join(paths::RUNTIME_FALLBACK_DIR);
            let _ = std::fs::create_dir_all(&home_dir);
            return home_dir;
        }

        system_dir
    }

    /// Download a runtime binary. Tries the preferred runtime first, then fallback.
    async fn download_runtime(install_dir: &Path, preferred: &str) -> Result<PathBuf> {
        let arch = Self::detect_arch();

        // Order downloads based on preference
        let download_order: Vec<&str> = std::iter::once(preferred)
            .chain(
                ["youki", "crun"]
                    .iter()
                    .copied()
                    .filter(|r| *r != preferred),
            )
            .collect();

        for runtime in &download_order {
            let result = match *runtime {
                "youki" => Self::download_youki(install_dir, &arch).await,
                "crun" => Self::download_crun(install_dir, &arch).await,
                other => {
                    warn!("{} has no prebuilt binary — skipping auto-download", other);
                    continue;
                }
            };
            match result {
                Ok(path) => return Ok(path),
                Err(e) => warn!("Failed to download {}: {}", runtime, e),
            }
        }

        Err(anyhow::anyhow!(
            "Failed to auto-download any OCI runtime. Install youki or crun manually."
        ))
    }

    /// Download youki from GitHub Releases.
    /// Asset: youki-{version}-{arch}-musl.tar.gz
    async fn download_youki(install_dir: &Path, arch: &str) -> Result<PathBuf> {
        let youki_arch = match arch {
            "x86_64" => "x86_64",
            "aarch64" => "aarch64",
            _ => anyhow::bail!("Unsupported architecture for youki: {}", arch),
        };

        let url = format!(
            "https://github.com/youki-dev/youki/releases/download/{}/youki-{}-{}-musl.tar.gz",
            runtime::YOUKI_VERSION,
            runtime::YOUKI_VERSION.trim_start_matches('v'),
            youki_arch
        );

        info!("Downloading youki {} from {}", runtime::YOUKI_VERSION, url);

        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(10))
            .build()?;

        let response = client.get(&url).send().await?;
        if !response.status().is_success() {
            anyhow::bail!("HTTP {}: {}", response.status(), url);
        }

        let bytes = response.bytes().await?;
        info!("Downloaded {} bytes", bytes.len());

        // Extract tar.gz
        let dest = install_dir.join("youki");
        tokio::task::spawn_blocking({
            let bytes = bytes.to_vec();
            let dest = dest.clone();
            let install_dir = install_dir.to_path_buf();
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

                    if filename == "youki" {
                        entry.unpack(&dest)?;

                        // Make executable
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            std::fs::set_permissions(
                                &dest,
                                std::fs::Permissions::from_mode(0o755),
                            )?;
                        }

                        info!("Extracted youki to {}", dest.display());
                        break;
                    } else {
                        // Extract other binaries too (youki-integration-test etc.)
                        let out_path = install_dir.join(&filename);
                        let _ = entry.unpack(&out_path);
                    }
                }

                Ok(())
            }
        })
        .await??;

        if !dest.exists() {
            anyhow::bail!("youki binary not found in archive");
        }

        // Save version info
        Self::save_version_info(install_dir, "youki", runtime::YOUKI_VERSION).await?;

        Ok(dest)
    }

    /// Download crun from GitHub Releases.
    /// Asset: crun-{version}-linux-{arch} (bare binary)
    async fn download_crun(install_dir: &Path, arch: &str) -> Result<PathBuf> {
        let crun_arch = match arch {
            "x86_64" => "amd64",
            "aarch64" => "arm64",
            _ => anyhow::bail!("Unsupported architecture for crun: {}", arch),
        };

        let url = format!(
            "https://github.com/containers/crun/releases/download/{}/crun-{}-linux-{}",
            runtime::CRUN_VERSION,
            runtime::CRUN_VERSION,
            crun_arch
        );

        info!("Downloading crun {} from {}", runtime::CRUN_VERSION, url);

        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(10))
            .build()?;

        let response = client.get(&url).send().await?;
        if !response.status().is_success() {
            anyhow::bail!("HTTP {}: {}", response.status(), url);
        }

        let bytes = response.bytes().await?;
        info!("Downloaded {} bytes", bytes.len());

        let dest = install_dir.join("crun");
        tokio::fs::write(&dest, &bytes).await?;

        // Make executable
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))?;
        }

        info!("Downloaded crun to {}", dest.display());

        // Save version info
        Self::save_version_info(install_dir, "crun", runtime::CRUN_VERSION).await?;

        Ok(dest)
    }

    /// Detect the CPU architecture.
    fn detect_arch() -> String {
        std::env::consts::ARCH.to_string()
    }

    /// Save version info to a JSON file for tracking.
    async fn save_version_info(install_dir: &Path, name: &str, version: &str) -> Result<()> {
        let version_file = install_dir.join("runtime-version.json");
        let mut versions: serde_json::Value = if version_file.exists() {
            let data = tokio::fs::read_to_string(&version_file).await?;
            serde_json::from_str(&data).unwrap_or(serde_json::json!({}))
        } else {
            serde_json::json!({})
        };

        versions[name] = serde_json::json!({
            "version": version,
            "installed_at": chrono::Utc::now().to_rfc3339(),
            "arch": Self::detect_arch(),
        });

        tokio::fs::write(&version_file, serde_json::to_string_pretty(&versions)?).await?;
        Ok(())
    }

    /// Get information about installed runtimes.
    pub async fn get_installed_info(install_dir: Option<&str>) -> Result<serde_json::Value> {
        let dir = install_dir
            .map(PathBuf::from)
            .unwrap_or_else(Self::install_dir);
        let version_file = dir.join("runtime-version.json");

        if version_file.exists() {
            let data = tokio::fs::read_to_string(&version_file).await?;
            Ok(serde_json::from_str(&data)?)
        } else {
            Ok(serde_json::json!({}))
        }
    }
}
