use std::io::Write;
use std::path::Path;

use crate::oci_spec::OciSpec;
use crate::VIRTIOFS_TAG;

/// Paths where the host may mount the OCI config via virtio-fs or 9p.
pub const CONFIG_PATHS: &[&str] = &[
    "/run/config.json",
    "/mnt/config.json",
    "/config.json",
    "/mnt/rootfs/config.json", // virtio-fs mounted rootfs
];

/// Mount essential pseudo-filesystems.
#[cfg(target_os = "linux")]
pub fn mount_filesystems() -> Result<(), Box<dyn std::error::Error>> {
    use std::ffi::CString;
    use std::fs;

    // Create mount points if they don't exist
    let dirs = [
        "/proc", "/sys", "/dev", "/dev/pts", "/dev/shm", "/tmp", "/run",
    ];
    for dir in &dirs {
        if !Path::new(dir).exists() {
            fs::create_dir_all(dir)?;
        }
    }

    // Mount table: (source, target, fstype, flags)
    let ms_nosuid = libc::MS_NOSUID as u64;
    let ms_nodev = libc::MS_NODEV as u64;
    let ms_noexec = libc::MS_NOEXEC as u64;

    let mounts: &[(&str, &str, &str, u64)] = &[
        ("proc", "/proc", "proc", ms_nosuid | ms_nodev | ms_noexec),
        ("sysfs", "/sys", "sysfs", ms_nosuid | ms_nodev | ms_noexec),
        ("devtmpfs", "/dev", "devtmpfs", ms_nosuid),
        ("devpts", "/dev/pts", "devpts", ms_nosuid | ms_noexec),
        ("tmpfs", "/dev/shm", "tmpfs", ms_nosuid | ms_nodev),
        ("tmpfs", "/tmp", "tmpfs", ms_nosuid | ms_nodev),
        ("tmpfs", "/run", "tmpfs", ms_nosuid | ms_nodev | ms_noexec),
    ];

    for (source, target, fstype, flags) in mounts {
        if is_mounted(target) {
            continue;
        }

        let c_source = CString::new(*source)?;
        let c_target = CString::new(*target)?;
        let c_fstype = CString::new(*fstype)?;

        let ret = unsafe {
            libc::mount(
                c_source.as_ptr(),
                c_target.as_ptr(),
                c_fstype.as_ptr(),
                *flags,
                std::ptr::null(),
            )
        };

        if ret != 0 {
            let err = std::io::Error::last_os_error();
            log_error!("mount {} on {} failed: {}", source, target, err);
        }

        // After devtmpfs replaces /dev, recreate mount points for devpts/shm.
        if *target == "/dev" && ret == 0 {
            let _ = fs::create_dir_all("/dev/pts");
            let _ = fs::create_dir_all("/dev/shm");
        }
    }

    // Create standard symlinks in /dev
    create_dev_symlinks();

    log_info!("pseudo-filesystems mounted");
    Ok(())
}

#[cfg(target_os = "linux")]
fn is_mounted(target: &str) -> bool {
    std::fs::read_to_string("/proc/mounts")
        .unwrap_or_default()
        .lines()
        .any(|line| {
            line.split_whitespace()
                .nth(1)
                .map_or(false, |mp| mp == target)
        })
}

#[cfg(target_os = "linux")]
fn create_dev_symlinks() {
    let symlinks = [
        ("/proc/self/fd", "/dev/fd"),
        ("/proc/self/fd/0", "/dev/stdin"),
        ("/proc/self/fd/1", "/dev/stdout"),
        ("/proc/self/fd/2", "/dev/stderr"),
    ];
    for (src, dst) in &symlinks {
        if !Path::new(dst).exists() {
            let _ = std::os::unix::fs::symlink(src, dst);
        }
    }
}

/// Mount the virtio-fs shared directory from the host.
///
/// k3rs-vmm shares the container rootfs directory via virtio-fs with tag "rootfs".
/// We mount it at /mnt/rootfs so the container filesystem from the host is accessible.
#[cfg(target_os = "linux")]
pub fn mount_virtiofs() {
    use std::ffi::CString;
    use std::fs;

    let mount_point = "/mnt/rootfs";
    if let Err(e) = fs::create_dir_all(mount_point) {
        log_error!("failed to create virtio-fs mount point: {}", e);
        return;
    }

    let c_source = match CString::new(VIRTIOFS_TAG) {
        Ok(s) => s,
        Err(_) => return,
    };
    let c_target = match CString::new(mount_point) {
        Ok(s) => s,
        Err(_) => return,
    };
    let c_fstype = match CString::new("virtiofs") {
        Ok(s) => s,
        Err(_) => return,
    };

    let ret = unsafe {
        libc::mount(
            c_source.as_ptr(),
            c_target.as_ptr(),
            c_fstype.as_ptr(),
            0,
            std::ptr::null(),
        )
    };

    if ret == 0 {
        log_info!("virtio-fs '{}' mounted at {}", VIRTIOFS_TAG, mount_point);
    } else {
        let err = std::io::Error::last_os_error();
        // Not a hard failure — VM might not have virtio-fs configured
        log_info!(
            "virtio-fs mount skipped ({}), not available in this VM",
            err
        );
    }
}

/// Load OCI config.json from one of the known paths.
pub fn load_oci_config() -> Result<OciSpec, Box<dyn std::error::Error>> {
    for path in CONFIG_PATHS {
        if Path::new(path).exists() {
            log_info!("loading OCI config from {}", path);
            let data = std::fs::read_to_string(path)?;
            let spec: OciSpec = serde_json::from_str(&data)?;
            return Ok(spec);
        }
    }
    Err("no config.json found at any known path".into())
}
