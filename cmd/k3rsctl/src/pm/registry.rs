use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

use super::types::PmRegistry;

/// Root directory for all PM state: `~/.k3rs/pm/`.
pub fn pm_base_dir() -> PathBuf {
    dirs::home_dir()
        .expect("could not determine home directory")
        .join(".k3rs")
        .join("pm")
}

pub fn bins_dir() -> PathBuf {
    pm_base_dir().join("bins")
}

pub fn pids_dir() -> PathBuf {
    pm_base_dir().join("pids")
}

pub fn logs_dir() -> PathBuf {
    pm_base_dir().join("logs")
}

pub fn configs_dir() -> PathBuf {
    pm_base_dir().join("configs")
}

fn registry_path() -> PathBuf {
    pm_base_dir().join("registry.json")
}

/// Create all PM subdirectories if they don't exist.
pub fn ensure_dirs() -> Result<()> {
    for dir in [bins_dir(), pids_dir(), logs_dir(), configs_dir()] {
        fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create directory {}", dir.display()))?;
    }
    Ok(())
}

/// Load the registry from disk, returning a default if the file doesn't exist.
pub fn load() -> Result<PmRegistry> {
    let path = registry_path();
    if !path.exists() {
        return Ok(PmRegistry::default());
    }
    let data =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let reg: PmRegistry =
        serde_json::from_str(&data).with_context(|| "failed to parse registry.json")?;
    Ok(reg)
}

/// Atomically write the registry to disk (write to `.tmp`, then rename).
pub fn save(registry: &PmRegistry) -> Result<()> {
    ensure_dirs()?;
    let path = registry_path();
    let tmp = path.with_extension("json.tmp");
    let data = serde_json::to_string_pretty(registry)?;
    fs::write(&tmp, data).with_context(|| "failed to write registry.json.tmp")?;
    fs::rename(&tmp, &path).with_context(|| "failed to rename registry.json.tmp")?;
    Ok(())
}

/// Load the registry, apply a mutation, and save it back atomically.
pub fn update<F: FnOnce(&mut PmRegistry)>(f: F) -> Result<()> {
    let mut reg = load()?;
    f(&mut reg);
    save(&reg)
}
