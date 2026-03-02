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
        bail!(
            "no install method specified for {}. Use --from-source or --bin-path.",
            key
        );
    }

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
