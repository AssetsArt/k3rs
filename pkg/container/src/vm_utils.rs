//! Shared utilities for VM backends (Virtualization.framework on macOS, Firecracker on Linux).
//!
//! Contains functions and types used by both platform-specific backends:
//! - `find_k3rs_init()`: Locate the k3rs-init binary for guest injection
//! - `parse_bundle_config()`: Parse OCI bundle config.json for entrypoint/env
//! - `VmNetworkConfig`: VPC networking parameters for a VM

use std::path::{Path, PathBuf};

use pkg_constants::paths::DATA_DIR;

/// VPC networking parameters for a VM (passed to k3rs-vmm as CLI args on macOS,
/// or to the guest kernel cmdline on Linux/Firecracker).
#[derive(Debug, Clone)]
pub struct VmNetworkConfig {
    pub guest_ipv4: String,
    pub guest_ipv6: String,
    pub vpc_id: u16,
    pub vpc_cidr: String,
    pub gw_mac: String,
    pub platform_prefix: u32,
    pub cluster_id: u32,
}

/// Locate the k3rs-init binary on the host.
///
/// Search order:
/// 1. `{DATA_DIR}/bin/k3rs-init` (system install)
/// 2. `~/.k3rs/bin/k3rs-init` (user install)
/// 3. Cargo build output (`./target/<arch>-unknown-linux-musl/{release,debug}/k3rs-init`)
pub(crate) fn find_k3rs_init() -> Option<PathBuf> {
    let system_path = format!("{}/bin/k3rs-init", DATA_DIR);
    if let Some(p) = try_path(&system_path) {
        return Some(p);
    }

    if let Some(home) = std::env::var_os("HOME") {
        let user_path = PathBuf::from(home).join(".k3rs/bin/k3rs-init");
        if user_path.exists() {
            return Some(user_path);
        }
    }

    // Cargo build outputs — aarch64 first (Apple Silicon M-series)
    for arch in &["aarch64", "x86_64"] {
        for profile in &["release", "debug"] {
            let p = format!("./target/{}-unknown-linux-musl/{}/k3rs-init", arch, profile);
            if let Some(path) = try_path(&p) {
                return Some(path);
            }
        }
    }

    None
}

fn try_path(s: &str) -> Option<PathBuf> {
    let p = PathBuf::from(s);
    if p.exists() { Some(p) } else { None }
}

/// Parse the OCI bundle config.json to extract command and env vars.
/// Returns empty vecs on parse failure (k3rs-init defaults to /bin/sh).
pub(crate) fn parse_bundle_config(bundle: &Path) -> (Vec<String>, Vec<String>) {
    let data = match std::fs::read_to_string(bundle.join("config.json")) {
        Ok(d) => d,
        Err(_) => return (Vec::new(), Vec::new()),
    };
    let v: serde_json::Value = match serde_json::from_str(&data) {
        Ok(v) => v,
        Err(_) => return (Vec::new(), Vec::new()),
    };

    let command = v["process"]["args"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let env = v["process"]["env"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    (command, env)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_bundle_config_missing() {
        let (cmd, env) = parse_bundle_config(Path::new("/nonexistent"));
        assert!(cmd.is_empty());
        assert!(env.is_empty());
    }

    #[test]
    fn test_parse_bundle_config_valid() {
        use std::io::Write;
        let tmp = std::env::temp_dir().join("k3rs-vm-utils-cfg-test");
        std::fs::create_dir_all(&tmp).unwrap();

        let cfg = serde_json::json!({
            "process": {
                "args": ["/bin/bash", "-l"],
                "env": ["HOME=/root", "PATH=/bin"]
            }
        });
        let mut f = std::fs::File::create(tmp.join("config.json")).unwrap();
        f.write_all(serde_json::to_string(&cfg).unwrap().as_bytes())
            .unwrap();

        let (cmd, env) = parse_bundle_config(&tmp);
        assert_eq!(cmd, vec!["/bin/bash", "-l"]);
        assert_eq!(env, vec!["HOME=/root", "PATH=/bin"]);

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
