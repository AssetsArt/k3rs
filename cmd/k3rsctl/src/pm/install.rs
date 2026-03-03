use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use super::registry;
use super::types::{ComponentName, ProcessEntry, ProcessStatus};

/// Install one or more components.
pub fn install(component: &ComponentName, from_source: bool, bin_path: Option<&str>) -> Result<()> {
    registry::ensure_dirs()?;

    for comp in component.resolve() {
        install_one(&comp, from_source, bin_path)?;
    }
    Ok(())
}

fn install_one(component: &ComponentName, from_source: bool, bin_path: Option<&str>) -> Result<()> {
    let key = component.key().to_string();
    println!("Installing {}...", key);

    let dest = registry::bins_dir().join(component.bin_name());

    if let Some(path) = bin_path {
        // Copy an existing binary
        let src = Path::new(path);
        if !src.exists() {
            bail!("binary not found: {}", path);
        }
        fs::copy(src, &dest)
            .with_context(|| format!("failed to copy {} to {}", path, dest.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dest, fs::Permissions::from_mode(0o755))?;
        }
        println!("  Copied {} -> {}", path, dest.display());
    } else if from_source {
        // Build from workspace source
        let workspace = find_workspace_root()?;
        println!("  Building from source in {}...", workspace.display());

        let status = Command::new("cargo")
            .args([
                "build",
                "--release",
                "-p",
                component.cargo_package(),
            ])
            .current_dir(&workspace)
            .status()
            .context("failed to run cargo build")?;

        if !status.success() {
            bail!(
                "cargo build failed for {} (exit code {:?})",
                component.cargo_package(),
                status.code()
            );
        }

        let built = workspace
            .join("target")
            .join("release")
            .join(component.bin_name());
        if !built.exists() {
            bail!("built binary not found at {}", built.display());
        }
        fs::copy(&built, &dest).with_context(|| {
            format!("failed to copy {} to {}", built.display(), dest.display())
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dest, fs::Permissions::from_mode(0o755))?;
        }
        println!("  Installed {} -> {}", built.display(), dest.display());
    } else {
        // Default: download pre-built binary from GitHub Releases
        download_from_github(component, &dest)?;
    }

    // Verify binary runs (--version check)
    verify_binary(&dest, component)?;

    // Generate a default config
    generate_default_config(component)?;

    // Register in the process registry
    let logs = registry::logs_dir();
    let config_path = registry::configs_dir().join(format!("{}.yaml", key));

    registry::update(|reg| {
        reg.processes.insert(
            key.clone(),
            ProcessEntry {
                name: key.clone(),
                bin_path: dest.clone(),
                args: Vec::new(),
                env: HashMap::new(),
                status: ProcessStatus::Stopped,
                pid: None,
                restart_count: 0,
                started_at: None,
                auto_restart: true,
                max_restarts: 10,
                config_path: Some(config_path),
                stdout_log: logs.join(format!("{}.log", key)),
                stderr_log: logs.join(format!("{}-error.log", key)),
            },
        );
    })?;

    println!("  {} registered (status: stopped)", component.bin_name());
    Ok(())
}

const GITHUB_REPO: &str = "AssetsArt/k3rs";

/// Download a pre-built binary from GitHub Releases.
///
/// Fetches the latest release tag, then downloads the binary for the current
/// architecture from `https://github.com/{REPO}/releases/download/{tag}/{bin}-{arch}`.
fn download_from_github(component: &ComponentName, dest: &Path) -> Result<()> {
    let bin = component.bin_name();
    let arch = std::env::consts::ARCH; // "x86_64" or "aarch64"
    let os = std::env::consts::OS; // "linux" or "macos"

    // Fetch latest release tag via GitHub API
    println!("  Fetching latest release from github.com/{}...", GITHUB_REPO);
    let tag_output = Command::new("curl")
        .args([
            "-sfL",
            &format!(
                "https://api.github.com/repos/{}/releases/latest",
                GITHUB_REPO
            ),
        ])
        .output()
        .context("failed to query GitHub API (is curl installed?)")?;

    if !tag_output.status.success() {
        bail!(
            "failed to fetch latest release from GitHub (HTTP error). \
             Use --from-source or --bin-path instead."
        );
    }

    let tag_json: serde_json::Value = serde_json::from_slice(&tag_output.stdout)
        .context("failed to parse GitHub release JSON")?;
    let tag = tag_json["tag_name"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("no tag_name in GitHub release response"))?;

    let asset_name = format!("{}-{}-{}", bin, os, arch);
    let url = format!(
        "https://github.com/{}/releases/download/{}/{}",
        GITHUB_REPO, tag, asset_name
    );

    println!("  Downloading {} ({})...", asset_name, tag);
    let dl_status = Command::new("curl")
        .args(["-sfL", "-o", &dest.to_string_lossy(), &url])
        .status()
        .context("failed to download binary (is curl installed?)")?;

    if !dl_status.success() {
        bail!(
            "download failed for {}. Binary may not be available for {}-{}. \
             Use --from-source to build locally.",
            url,
            os,
            arch
        );
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dest, fs::Permissions::from_mode(0o755))?;
    }

    println!("  Downloaded {} -> {}", asset_name, dest.display());
    Ok(())
}

/// Verify a binary works by running `<binary> --version`.
fn verify_binary(bin: &Path, component: &ComponentName) -> Result<()> {
    let output = Command::new(bin)
        .arg("--version")
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let version = String::from_utf8_lossy(&out.stdout);
            let version = version.trim();
            if version.is_empty() {
                let stderr = String::from_utf8_lossy(&out.stderr);
                println!("  Verified: {} ({})", component.bin_name(), stderr.trim());
            } else {
                println!("  Verified: {}", version);
            }
            Ok(())
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            eprintln!(
                "  Warning: {} --version exited with {:?}: {}",
                component.bin_name(),
                out.status.code(),
                stderr.trim()
            );
            Ok(()) // non-fatal — binary may not support --version
        }
        Err(e) => {
            eprintln!(
                "  Warning: could not verify {}: {}",
                component.bin_name(),
                e
            );
            Ok(()) // non-fatal
        }
    }
}

/// Walk up from CWD to find the workspace root (directory with `[workspace]` in Cargo.toml).
fn find_workspace_root() -> Result<PathBuf> {
    let mut dir = std::env::current_dir()?;
    loop {
        let manifest = dir.join("Cargo.toml");
        if manifest.exists() {
            let content = fs::read_to_string(&manifest)?;
            if content.contains("[workspace]") {
                return Ok(dir);
            }
        }
        if !dir.pop() {
            bail!("could not find workspace root (no Cargo.toml with [workspace] found)");
        }
    }
}

/// Write a minimal default config YAML for the component.
fn generate_default_config(component: &ComponentName) -> Result<()> {
    let key = component.key();
    let path = registry::configs_dir().join(format!("{}.yaml", key));
    if path.exists() {
        // Don't overwrite existing configs
        return Ok(());
    }

    let hostname = std::env::var("HOSTNAME")
        .or_else(|_| fs::read_to_string("/etc/hostname").map(|s| s.trim().to_string()))
        .unwrap_or_else(|_| "node-0".to_string());

    let data_dir = dirs::home_dir()
        .expect("could not determine home directory")
        .join(".k3rs")
        .join("data")
        .join(key);

    let content = match component {
        ComponentName::Server => format!(
            "# k3rs-server default config\n\
             port: 6443\n\
             data-dir: {}\n\
             node-name: {}\n",
            data_dir.display(),
            hostname,
        ),
        ComponentName::Agent => format!(
            "# k3rs-agent default config\n\
             server: http://127.0.0.1:6443\n\
             node-name: {}\n\
             data-dir: {}\n",
            hostname,
            data_dir.display(),
        ),
        ComponentName::Vpc => format!(
            "# k3rs-vpc default config\n\
             server-url: http://127.0.0.1:6443\n\
             data-dir: {}\n",
            data_dir.display(),
        ),
        ComponentName::Ui => "# k3rs-ui default config\n".to_string(),
        ComponentName::All => unreachable!(),
    };

    fs::write(&path, content)
        .with_context(|| format!("failed to write default config to {}", path.display()))?;
    println!("  Generated default config: {}", path.display());
    Ok(())
}
