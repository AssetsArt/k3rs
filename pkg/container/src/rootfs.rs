use anyhow::Result;
use std::path::{Path, PathBuf};
use tracing::info;

/// Check if we are running as root.
fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

/// Current user's UID for debugging.
fn host_uid() -> u32 {
    unsafe { libc::geteuid() }
}

/// Current user's GID for debugging.
fn host_gid() -> u32 {
    unsafe { libc::getegid() }
}

/// Parse entrypoint and cmd from OCI image config.json.
pub fn parse_image_config(image_dir: &Path) -> (Vec<String>, Vec<String>) {
    let config_path = image_dir.join("config.json");
    let data = match std::fs::read_to_string(&config_path) {
        Ok(d) => d,
        Err(_) => return (vec![], vec![]),
    };
    let v: serde_json::Value = match serde_json::from_str(&data) {
        Ok(v) => v,
        Err(_) => return (vec![], vec![]),
    };

    let config = v.get("config").or_else(|| v.get("Config"));
    let config = match config {
        Some(c) => c,
        None => return (vec![], vec![]),
    };

    let entrypoint = config
        .get("Entrypoint")
        .and_then(|e| e.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let cmd = config
        .get("Cmd")
        .and_then(|e| e.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    (entrypoint, cmd)
}

/// Parse environment variables from OCI image config.json.
pub fn parse_image_env(image_dir: &Path) -> Vec<String> {
    let config_path = image_dir.join("config.json");
    let data = match std::fs::read_to_string(&config_path) {
        Ok(d) => d,
        Err(_) => return vec![],
    };
    let v: serde_json::Value = match serde_json::from_str(&data) {
        Ok(v) => v,
        Err(_) => return vec![],
    };

    let config = v.get("config").or_else(|| v.get("Config"));
    config
        .and_then(|c| c.get("Env"))
        .and_then(|e| e.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Parse working directory from OCI image config.json.
pub fn parse_image_workdir(image_dir: &Path) -> Option<String> {
    let config_path = image_dir.join("config.json");
    let data = std::fs::read_to_string(&config_path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&data).ok()?;
    let config = v.get("config").or_else(|| v.get("Config"))?;
    config
        .get("WorkingDir")
        .and_then(|w| w.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// Container networking mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NetworkMode {
    /// Use host networking (no network namespace isolation).
    #[default]
    Host,
    /// Create isolated network namespace (requires CNI setup).
    Isolated,
}

/// Manages rootfs extraction from OCI image layers.
pub struct RootfsManager;

impl RootfsManager {
    /// Extract OCI image layers into a rootfs directory.
    pub async fn extract(image_dir: &Path, container_dir: &Path) -> Result<PathBuf> {
        let rootfs = container_dir.join("rootfs");
        tokio::fs::create_dir_all(&rootfs).await?;

        let layers_dir = image_dir.join("layers");
        if !layers_dir.exists() {
            anyhow::bail!("No layers directory found at {}", layers_dir.display());
        }

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
                archive.set_preserve_permissions(true);
                archive.set_overwrite(true);

                // Hard link entries (EntryType::Link) can appear before the file they point to
                // within the same tar. Collect them and retry after all regular entries are done.
                let mut deferred_hard_links: Vec<(PathBuf, PathBuf)> = Vec::new();

                for entry in archive.entries()? {
                    let mut entry = entry?;
                    let entry_path = entry.path()?.into_owned();

                    // Skip whiteout files (OCI layer deletion markers) — they start with ".wh."
                    if let Some(name) = entry_path.file_name().and_then(|n| n.to_str())
                        && name.starts_with(".wh.")
                    {
                        continue;
                    }

                    // Calculate safe destination path, avoiding absolute path traversal.
                    let mut dest = rootfs_clone.to_path_buf();
                    for component in entry_path.components() {
                        if let std::path::Component::Normal(c) = component {
                            dest.push(c);
                        }
                    }

                    // Always defer hard links. The tar crate's unpack() for EntryType::Link
                    // uses relative paths from the current working directory, which fails
                    // when extracting to a nested rootfs. We also need to ensure the source
                    // file exists, so we collect them and link them in a second pass using
                    // absolute paths.
                    if entry.header().entry_type() == tar::EntryType::Link {
                        if let Some(link_name) = entry.header().link_name()? {
                            let mut link_src = rootfs_clone.to_path_buf();
                            for component in link_name.components() {
                                if let std::path::Component::Normal(c) = component {
                                    link_src.push(c);
                                }
                            }
                            deferred_hard_links.push((link_src, dest));
                            continue;
                        }
                    }

                    // If an existing non-directory file blocks the unpack, remove it first.
                    // This handles the case where a previous layer wrote a file with
                    // restrictive permissions (e.g. 0555) that the tar crate cannot overwrite.
                    if dest.exists() && !dest.is_dir() {
                        // Best-effort chmod to make it writable, then remove.
                        let _ = std::fs::set_permissions(
                            &dest,
                            std::os::unix::fs::PermissionsExt::from_mode(0o644),
                        );
                        let _ = std::fs::remove_file(&dest);
                    }

                    // Ensure parent directory exists.
                    if let Some(parent) = dest.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }

                    if let Err(e) = entry.unpack(&dest) {
                        // EEXIST on directories is fine — the dir already exists.
                        // AlreadyExists can also surface for hard-linked entries on some kernels.
                        if e.kind() == std::io::ErrorKind::AlreadyExists {
                            continue;
                        }
                        return Err(anyhow::anyhow!(
                            "failed to unpack `{}`: {e}",
                            dest.display()
                        ));
                    }
                }

                // Retry deferred hard links now that all regular files are on disk.
                for (link_src, dest) in deferred_hard_links {
                    if dest.exists() {
                        continue; // a later layer or earlier iteration already wrote it
                    }
                    if link_src.exists() {
                        // Ensure parent directory exists for the hard link.
                        if let Some(parent) = dest.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }

                        std::fs::hard_link(&link_src, &dest).map_err(|e| {
                            anyhow::anyhow!(
                                "failed to hard link `{}` -> `{}`: {e}",
                                link_src.display(),
                                dest.display()
                            )
                        })?;
                    }
                    // If link_src still doesn't exist it wasn't in this layer set — skip.
                }

                Ok(())
            })
            .await??;
        }

        info!("Rootfs extracted to {}", rootfs.display());
        Ok(rootfs)
    }

    /// Generate OCI config.json — backward-compatible wrapper.
    pub fn generate_config(
        container_id: &str,
        rootfs_path: &Path,
        command: &[String],
        env_vars: &std::collections::HashMap<String, String>,
    ) -> Result<String> {
        Self::generate_config_full(
            container_id,
            rootfs_path,
            command,
            env_vars,
            None,
            None,
            NetworkMode::default(),
        )
    }

    /// Full config generation with image config support and network mode.
    pub fn generate_config_full(
        container_id: &str,
        _rootfs_path: &Path,
        command: &[String],
        env_vars: &std::collections::HashMap<String, String>,
        image_dir: Option<&Path>,
        working_dir: Option<&str>,
        network_mode: NetworkMode,
    ) -> Result<String> {
        // Resolve command: pod spec > image entrypoint+cmd > /bin/sh
        let cmd: Vec<String> = if !command.is_empty() {
            command.to_vec()
        } else if let Some(img_dir) = image_dir {
            let (entrypoint, img_cmd) = parse_image_config(img_dir);
            if !entrypoint.is_empty() {
                let mut full = entrypoint;
                full.extend(img_cmd);
                full
            } else if !img_cmd.is_empty() {
                img_cmd
            } else {
                vec!["/bin/sh".to_string()]
            }
        } else {
            vec!["/bin/sh".to_string()]
        };

        // Resolve working directory
        let cwd = working_dir
            .map(String::from)
            .or_else(|| image_dir.and_then(parse_image_workdir))
            .unwrap_or_else(|| "/".to_string());

        // Build environment: image env (base) + defaults + user-specified (override)
        let mut env_map: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();

        // 1. Image environment (lowest priority)
        if let Some(img_dir) = image_dir {
            for entry in parse_image_env(img_dir) {
                if let Some((k, v)) = entry.split_once('=') {
                    env_map.insert(k.to_string(), v.to_string());
                }
            }
        }

        // 2. Defaults
        env_map.entry("PATH".to_string()).or_insert_with(|| {
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string()
        });
        env_map.insert("HOSTNAME".to_string(), container_id.to_string());
        env_map
            .entry("TERM".to_string())
            .or_insert_with(|| "xterm".to_string());
        env_map
            .entry("HOME".to_string())
            .or_insert_with(|| "/root".to_string());

        // 3. User-specified env (highest priority)
        for (k, v) in env_vars {
            env_map.insert(k.clone(), v.clone());
        }

        let env: Vec<String> = env_map
            .into_iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect();

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

        // Build namespaces - no user namespace by default (avoid rootless permission issues)
        let mut namespaces = vec![
            serde_json::json!({ "type": "pid" }),
            serde_json::json!({ "type": "ipc" }),
            serde_json::json!({ "type": "uts" }),
            serde_json::json!({ "type": "mount" }),
            serde_json::json!({ "type": "cgroup" }),
        ];

        if network_mode == NetworkMode::Isolated {
            namespaces.push(serde_json::json!({ "type": "network" }));
        }

        // Create basic Linux section without user namespace
        let linux = serde_json::json!({
            "namespaces": namespaces,
            "maskedPaths": [
                "/proc/acpi", "/proc/asound", "/proc/kcore", "/proc/keys",
                "/proc/latency_stats", "/proc/timer_list", "/proc/timer_stats",
                "/proc/sched_debug", "/sys/firmware", "/proc/scsi"
            ],
            "readonlyPaths": [
                "/proc/bus", "/proc/fs", "/proc/irq", "/proc/sys", "/proc/sysrq-trigger"
            ]
        });

        let config = serde_json::json!({
            "ociVersion": "1.0.2",
            "process": {
                "terminal": false,
                "user": { "uid": 0, "gid": 0 },
                "args": cmd,
                "env": env,
                "cwd": cwd,
                "capabilities": {
                    "bounding": default_caps,
                    "effective": default_caps,
                    "inheritable": default_caps,
                    "permitted": default_caps,
                    "ambient": default_caps,
                },
                "rlimits": [{ "type": "RLIMIT_NOFILE", "hard": 1024u64, "soft": 1024u64 }],
                "noNewPrivileges": true
            },
            "root": { "path": "rootfs", "readonly": false },
            "hostname": container_id,
            "mounts": [
                { "destination": "/proc", "type": "proc", "source": "proc" },
                { "destination": "/dev", "type": "tmpfs", "source": "tmpfs",
                  "options": ["nosuid", "strictatime", "mode=755", "size=65536k"] },
                { "destination": "/dev/pts", "type": "devpts", "source": "devpts",
                  "options": ["nosuid", "noexec", "newinstance", "ptmxmode=0666", "mode=0620"] },
                { "destination": "/dev/shm", "type": "tmpfs", "source": "shm",
                  "options": ["nosuid", "noexec", "nodev", "mode=1777", "size=65536k"] },
                { "destination": "/dev/mqueue", "type": "mqueue", "source": "mqueue",
                  "options": ["nosuid", "noexec", "nodev"] },
                { "destination": "/sys", "type": "sysfs", "source": "sysfs",
                  "options": ["nosuid", "noexec", "nodev", "ro"] },
                { "destination": "/sys/fs/cgroup", "type": "cgroup", "source": "cgroup",
                  "options": ["nosuid", "noexec", "nodev", "relatime", "ro"] }
            ],
            "linux": linux
        });

        Ok(serde_json::to_string_pretty(&config)?)
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

        assert_eq!(config["ociVersion"], "1.0.2");
        assert_eq!(config["process"]["args"][0], "/bin/sh");
        assert_eq!(config["hostname"], "test-container");

        let caps = config["process"]["capabilities"]["bounding"]
            .as_array()
            .unwrap();
        assert!(caps.iter().any(|c| c == "CAP_NET_BIND_SERVICE"));
        assert!(caps.iter().any(|c| c == "CAP_CHOWN"));
        assert!(config["process"]["rlimits"].is_array());
        assert_eq!(config["process"]["rlimits"][0]["type"], "RLIMIT_NOFILE");
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
        assert!(mount_dests.contains(&"/sys"));
        assert!(mount_dests.contains(&"/sys/fs/cgroup"));
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

        let masked = config["linux"]["maskedPaths"].as_array().unwrap();
        let masked_strs: Vec<&str> = masked.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(masked_strs.contains(&"/proc/kcore"));
        assert!(masked_strs.contains(&"/proc/keys"));

        let readonly = config["linux"]["readonlyPaths"].as_array().unwrap();
        let readonly_strs: Vec<&str> = readonly.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(readonly_strs.contains(&"/proc/bus"));
        assert!(readonly_strs.contains(&"/proc/sys"));

        let namespaces = config["linux"]["namespaces"].as_array().unwrap();
        let ns_types: Vec<&str> = namespaces
            .iter()
            .map(|n| n["type"].as_str().unwrap())
            .collect();
        assert!(ns_types.contains(&"pid"));
        assert!(ns_types.contains(&"mount"));
    }

    #[test]
    fn test_generate_config_host_networking() {
        let config_str = RootfsManager::generate_config_full(
            "host-net",
            Path::new("/tmp/rootfs"),
            &[],
            &HashMap::new(),
            None,
            None,
            NetworkMode::Host,
        )
        .unwrap();
        let config: serde_json::Value = serde_json::from_str(&config_str).unwrap();
        let namespaces = config["linux"]["namespaces"].as_array().unwrap();
        let ns_types: Vec<&str> = namespaces
            .iter()
            .map(|n| n["type"].as_str().unwrap())
            .collect();
        assert!(!ns_types.contains(&"network"));
    }

    #[test]
    fn test_generate_config_isolated_networking() {
        let config_str = RootfsManager::generate_config_full(
            "iso-net",
            Path::new("/tmp/rootfs"),
            &[],
            &HashMap::new(),
            None,
            None,
            NetworkMode::Isolated,
        )
        .unwrap();
        let config: serde_json::Value = serde_json::from_str(&config_str).unwrap();
        let namespaces = config["linux"]["namespaces"].as_array().unwrap();
        let ns_types: Vec<&str> = namespaces
            .iter()
            .map(|n| n["type"].as_str().unwrap())
            .collect();
        assert!(ns_types.contains(&"network"));
    }

    #[test]
    fn test_generate_config_with_workdir() {
        let config_str = RootfsManager::generate_config_full(
            "workdir-test",
            Path::new("/tmp/rootfs"),
            &[],
            &HashMap::new(),
            None,
            Some("/app"),
            NetworkMode::default(),
        )
        .unwrap();
        let config: serde_json::Value = serde_json::from_str(&config_str).unwrap();
        assert_eq!(config["process"]["cwd"], "/app");
    }
}
use flate2::write::GzEncoder;
use flate2::Compression;
use tar::{Builder, Header, EntryType};

#[tokio::test]
async fn test_extract_hard_links() {
    let tmp_base = std::env::temp_dir().join(format!("k3rs-test-{}", chrono::Utc::now().timestamp_millis()));
    let _ = std::fs::remove_dir_all(&tmp_base);
    std::fs::create_dir_all(&tmp_base).unwrap();
    let image_dir = tmp_base.join("image");
    let layers_dir = image_dir.join("layers");
    std::fs::create_dir_all(&layers_dir).unwrap();
    
    let container_dir = tmp_base.join("container");
    std::fs::create_dir_all(&container_dir).unwrap();

    // Create a layer with a directory and a file, and a hard link to that file.
    let layer_path = layers_dir.join("layer_0.tar.gz");
    let file = std::fs::File::create(&layer_path).unwrap();
    let enc = GzEncoder::new(file, Compression::default());
    let mut tar = Builder::new(enc);

    // 1. Regular file: usr/bin/perl5.40.1
    let mut header = Header::new_gnu();
    let data = b"perl-binary-content";
    header.set_size(data.len() as u64);
    header.set_mode(0o755);
    tar.append_data(&mut header, "usr/bin/perl5.40.1", &data[..]).unwrap();

    // 2. Hard link: usr/bin/perl -> usr/bin/perl5.40.1
    let mut header = Header::new_gnu();
    header.set_entry_type(EntryType::Link);
    header.set_link_name("usr/bin/perl5.40.1").unwrap();
    header.set_size(0);
    tar.append_data(&mut header, "usr/bin/perl", &[][..]).unwrap();

    tar.into_inner().unwrap().finish().unwrap();

    // Also need a config.json because parse_image_workdir looks for it (though we don't call it here)
    std::fs::write(image_dir.join("config.json"), "{}").unwrap();

    // Extract
    let result = RootfsManager::extract(&image_dir, &container_dir).await;
    assert!(result.is_ok(), "Extraction failed: {:?}", result.err());

    let rootfs = result.unwrap();
    assert!(rootfs.join("usr/bin/perl5.40.1").exists());
    assert!(rootfs.join("usr/bin/perl").exists());
    
    // Verify it's a hard link (same inode)
    let meta1 = std::fs::metadata(rootfs.join("usr/bin/perl5.40.1")).unwrap();
    let meta2 = std::fs::metadata(rootfs.join("usr/bin/perl")).unwrap();
    
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(meta1.ino(), meta2.ino());
    }
    let _ = std::fs::remove_dir_all(&tmp_base);
}

#[tokio::test]
async fn test_extract_hard_links_deferred() {
    let tmp_base = std::env::temp_dir().join(format!("k3rs-test-deferred-{}", chrono::Utc::now().timestamp_millis()));
    let _ = std::fs::remove_dir_all(&tmp_base);
    std::fs::create_dir_all(&tmp_base).unwrap();
    let image_dir = tmp_base.join("image");
    let layers_dir = image_dir.join("layers");
    std::fs::create_dir_all(&layers_dir).unwrap();
    
    let container_dir = tmp_base.join("container");
    std::fs::create_dir_all(&container_dir).unwrap();

    // Create a layer where the hard link comes BEFORE the target file.
    let layer_path = layers_dir.join("layer_0.tar.gz");
    let file = std::fs::File::create(&layer_path).unwrap();
    let enc = GzEncoder::new(file, Compression::default());
    let mut tar = Builder::new(enc);

    // 1. Hard link: usr/bin/perl -> usr/bin/perl5.40.1 (Target doesn't exist yet!)
    let mut header = Header::new_gnu();
    header.set_entry_type(EntryType::Link);
    header.set_link_name("usr/bin/perl5.40.1").unwrap();
    header.set_size(0);
    tar.append_data(&mut header, "usr/bin/perl", &[][..]).unwrap();

    // 2. Regular file: usr/bin/perl5.40.1
    let mut header = Header::new_gnu();
    let data = b"perl-binary-content";
    header.set_size(data.len() as u64);
    header.set_mode(0o755);
    tar.append_data(&mut header, "usr/bin/perl5.40.1", &data[..]).unwrap();

    tar.into_inner().unwrap().finish().unwrap();
    std::fs::write(image_dir.join("config.json"), "{}").unwrap();

    // Extract
    let result = RootfsManager::extract(&image_dir, &container_dir).await;
    assert!(result.is_ok(), "Extraction failed: {:?}", result.err());

    let rootfs = result.unwrap();
    assert!(rootfs.join("usr/bin/perl5.40.1").exists());
    assert!(rootfs.join("usr/bin/perl").exists());
    
    // Verify it's a hard link (same inode)
    let meta1 = std::fs::metadata(rootfs.join("usr/bin/perl5.40.1")).unwrap();
    let meta2 = std::fs::metadata(rootfs.join("usr/bin/perl")).unwrap();
    
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(meta1.ino(), meta2.ino());
    }
    let _ = std::fs::remove_dir_all(&tmp_base);
}
