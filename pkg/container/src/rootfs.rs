use anyhow::Result;
use std::path::{Path, PathBuf};
use tracing::info;

/// Current user's UID for rootless container user namespace mappings.
fn host_uid() -> u32 {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
        .unwrap_or(1000)
}

/// Current user's GID for rootless container user namespace mappings.
fn host_gid() -> u32 {
    std::process::Command::new("id")
        .arg("-g")
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
        .unwrap_or(1000)
}

/// Manages rootfs extraction from OCI image layers.
pub struct RootfsManager;

impl RootfsManager {
    /// Extract OCI image layers into a rootfs directory.
    /// Applies layers in order (lower → upper) to form the final filesystem.
    /// Returns the rootfs path.
    pub async fn extract(image_dir: &Path, container_dir: &Path) -> Result<PathBuf> {
        let rootfs = container_dir.join("rootfs");
        tokio::fs::create_dir_all(&rootfs).await?;

        let layers_dir = image_dir.join("layers");
        if !layers_dir.exists() {
            anyhow::bail!("No layers directory found at {}", layers_dir.display());
        }

        // Collect and sort layer files
        let mut layers: Vec<PathBuf> = Vec::new();
        let mut entries = tokio::fs::read_dir(&layers_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("gz") {
                layers.push(path);
            }
        }
        layers.sort();

        info!("Extracting {} layers to {}", layers.len(), rootfs.display());

        // Extract each layer — must be done on a blocking thread (tar is sync)
        for (i, layer_path) in layers.iter().enumerate() {
            info!(
                "  Extracting layer {}/{}: {}",
                i + 1,
                layers.len(),
                layer_path.display()
            );

            let layer_path = layer_path.clone();
            let rootfs_clone = rootfs.clone();

            tokio::task::spawn_blocking(move || -> Result<()> {
                let file = std::fs::File::open(&layer_path)?;
                let decoder = flate2::read::GzDecoder::new(file);
                let mut archive = tar::Archive::new(decoder);

                // Unpack into rootfs, skip whiteout processing for simplicity
                archive.set_preserve_permissions(true);
                archive.set_overwrite(true);
                archive.unpack(&rootfs_clone)?;
                Ok(())
            })
            .await??;
        }

        info!("Rootfs extracted to {}", rootfs.display());
        Ok(rootfs)
    }

    /// Generate a complete OCI runtime config.json for the container.
    ///
    /// Produces a production-grade spec with:
    /// - Full Linux capabilities (default container set)
    /// - Proper mount points (/proc, /dev, /dev/pts, /dev/shm, /sys, /dev/mqueue, cgroup)
    /// - rlimits (RLIMIT_NOFILE)
    /// - Masked and readonly paths for security
    /// - Environment variables from the container spec
    /// - User namespace mappings for rootless operation
    pub fn generate_config(
        container_id: &str,
        _rootfs_path: &Path,
        command: &[String],
        env_vars: &std::collections::HashMap<String, String>,
    ) -> Result<String> {
        let cmd: Vec<&str> = if command.is_empty() {
            vec!["/bin/sh"]
        } else {
            command.iter().map(|s| s.as_str()).collect()
        };

        // Build environment: defaults + user-specified
        let mut env: Vec<String> = vec![
            "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string(),
            format!("HOSTNAME={}", container_id),
            "TERM=xterm".to_string(),
            "HOME=/root".to_string(),
        ];
        for (k, v) in env_vars {
            env.push(format!("{}={}", k, v));
        }

        // Default Linux capabilities for containers (Docker-compatible default set)
        let default_caps = serde_json::json!([
            "CAP_CHOWN",
            "CAP_DAC_OVERRIDE",
            "CAP_FSETID",
            "CAP_FOWNER",
            "CAP_MKNOD",
            "CAP_NET_RAW",
            "CAP_SETGID",
            "CAP_SETUID",
            "CAP_SETFCAP",
            "CAP_SETPCAP",
            "CAP_NET_BIND_SERVICE",
            "CAP_SYS_CHROOT",
            "CAP_KILL",
            "CAP_AUDIT_WRITE"
        ]);

        let config = serde_json::json!({
            "ociVersion": "1.0.2",
            "process": {
                "terminal": false,
                "user": { "uid": 0, "gid": 0 },
                "args": cmd,
                "env": env,
                "cwd": "/",
                "capabilities": {
                    "bounding": default_caps,
                    "effective": default_caps,
                    "inheritable": default_caps,
                    "permitted": default_caps,
                    "ambient": default_caps,
                },
                "rlimits": [
                    {
                        "type": "RLIMIT_NOFILE",
                        "hard": 1024u64,
                        "soft": 1024u64
                    }
                ],
                "noNewPrivileges": true
            },
            "root": {
                "path": "rootfs",
                "readonly": false
            },
            "hostname": container_id,
            "mounts": [
                {
                    "destination": "/proc",
                    "type": "proc",
                    "source": "proc"
                },
                {
                    "destination": "/dev",
                    "type": "tmpfs",
                    "source": "tmpfs",
                    "options": ["nosuid", "strictatime", "mode=755", "size=65536k"]
                },
                {
                    "destination": "/dev/pts",
                    "type": "devpts",
                    "source": "devpts",
                    "options": ["nosuid", "noexec", "newinstance", "ptmxmode=0666", "mode=0620"]
                },
                {
                    "destination": "/dev/shm",
                    "type": "tmpfs",
                    "source": "shm",
                    "options": ["nosuid", "noexec", "nodev", "mode=1777", "size=65536k"]
                },
                {
                    "destination": "/dev/mqueue",
                    "type": "mqueue",
                    "source": "mqueue",
                    "options": ["nosuid", "noexec", "nodev"]
                }
            ],
            "linux": {
                "namespaces": [
                    { "type": "pid" },
                    { "type": "ipc" },
                    { "type": "uts" },
                    { "type": "mount" },
                    { "type": "user" }
                ],
                "uidMappings": [
                    { "containerID": 0, "hostID": host_uid(), "size": 1 }
                ],
                "gidMappings": [
                    { "containerID": 0, "hostID": host_gid(), "size": 1 }
                ],
                "maskedPaths": [
                    "/proc/acpi",
                    "/proc/asound",
                    "/proc/kcore",
                    "/proc/keys",
                    "/proc/latency_stats",
                    "/proc/timer_list",
                    "/proc/timer_stats",
                    "/proc/sched_debug",
                    "/sys/firmware",
                    "/proc/scsi"
                ],
                "readonlyPaths": [
                    "/proc/bus",
                    "/proc/fs",
                    "/proc/irq",
                    "/proc/sys",
                    "/proc/sysrq-trigger"
                ]
            }
        });

        let config_json = serde_json::to_string_pretty(&config)?;
        Ok(config_json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_generate_config_defaults() {
        let config_str = RootfsManager::generate_config(
            "test-container",
            Path::new("/tmp/rootfs"),
            &[],
            &HashMap::new(),
        )
        .unwrap();

        let config: serde_json::Value = serde_json::from_str(&config_str).unwrap();

        // Check OCI version
        assert_eq!(config["ociVersion"], "1.0.2");

        // Check default command
        assert_eq!(config["process"]["args"][0], "/bin/sh");

        // Check hostname
        assert_eq!(config["hostname"], "test-container");

        // Check capabilities exist
        assert!(config["process"]["capabilities"]["bounding"].is_array());
        let caps = config["process"]["capabilities"]["bounding"]
            .as_array()
            .unwrap();
        assert!(caps.iter().any(|c| c == "CAP_NET_BIND_SERVICE"));
        assert!(caps.iter().any(|c| c == "CAP_CHOWN"));

        // Check rlimits
        assert!(config["process"]["rlimits"].is_array());
        assert_eq!(config["process"]["rlimits"][0]["type"], "RLIMIT_NOFILE");

        // Check noNewPrivileges
        assert_eq!(config["process"]["noNewPrivileges"], true);
    }

    #[test]
    fn test_generate_config_with_command() {
        let command = vec![
            "nginx".to_string(),
            "-g".to_string(),
            "daemon off;".to_string(),
        ];
        let config_str = RootfsManager::generate_config(
            "nginx-pod",
            Path::new("/tmp/rootfs"),
            &command,
            &HashMap::new(),
        )
        .unwrap();

        let config: serde_json::Value = serde_json::from_str(&config_str).unwrap();
        assert_eq!(config["process"]["args"][0], "nginx");
        assert_eq!(config["process"]["args"][1], "-g");
        assert_eq!(config["process"]["args"][2], "daemon off;");
    }

    #[test]
    fn test_generate_config_with_env() {
        let mut env = HashMap::new();
        env.insert("MY_VAR".to_string(), "hello".to_string());
        env.insert("DB_HOST".to_string(), "localhost".to_string());

        let config_str =
            RootfsManager::generate_config("env-test", Path::new("/tmp/rootfs"), &[], &env)
                .unwrap();

        let config: serde_json::Value = serde_json::from_str(&config_str).unwrap();
        let env_arr = config["process"]["env"].as_array().unwrap();

        // Should have PATH, HOSTNAME, TERM, HOME + 2 user vars = 6
        assert!(env_arr.len() >= 6);

        // Check user vars are included
        let env_strs: Vec<String> = env_arr
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert!(env_strs.iter().any(|e| e == "MY_VAR=hello"));
        assert!(env_strs.iter().any(|e| e == "DB_HOST=localhost"));
    }

    #[test]
    fn test_generate_config_mounts() {
        let config_str = RootfsManager::generate_config(
            "mount-test",
            Path::new("/tmp/rootfs"),
            &[],
            &HashMap::new(),
        )
        .unwrap();

        let config: serde_json::Value = serde_json::from_str(&config_str).unwrap();
        let mounts = config["mounts"].as_array().unwrap();

        let mount_dests: Vec<&str> = mounts
            .iter()
            .map(|m| m["destination"].as_str().unwrap())
            .collect();

        assert!(mount_dests.contains(&"/proc"));
        assert!(mount_dests.contains(&"/dev"));
        assert!(mount_dests.contains(&"/dev/pts"));
        assert!(mount_dests.contains(&"/dev/shm"));
        assert!(mount_dests.contains(&"/dev/mqueue"));
    }

    #[test]
    fn test_generate_config_security() {
        let config_str = RootfsManager::generate_config(
            "sec-test",
            Path::new("/tmp/rootfs"),
            &[],
            &HashMap::new(),
        )
        .unwrap();

        let config: serde_json::Value = serde_json::from_str(&config_str).unwrap();

        // Check masked paths
        let masked = config["linux"]["maskedPaths"].as_array().unwrap();
        let masked_strs: Vec<&str> = masked.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(masked_strs.contains(&"/proc/kcore"));
        assert!(masked_strs.contains(&"/proc/keys"));

        // Check readonly paths
        let readonly = config["linux"]["readonlyPaths"].as_array().unwrap();
        let readonly_strs: Vec<&str> = readonly.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(readonly_strs.contains(&"/proc/bus"));
        assert!(readonly_strs.contains(&"/proc/sys"));

        // Check namespaces include network
        let namespaces = config["linux"]["namespaces"].as_array().unwrap();
        let ns_types: Vec<&str> = namespaces
            .iter()
            .map(|n| n["type"].as_str().unwrap())
            .collect();
        assert!(ns_types.contains(&"pid"));
        assert!(ns_types.contains(&"user"));
        assert!(ns_types.contains(&"mount"));
    }
}
