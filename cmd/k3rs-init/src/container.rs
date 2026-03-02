use std::io::Write;
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

use crate::is_initrd_mode;
use crate::oci_spec::OciProcess;
use crate::signals::{reap_zombies, reaper_loop};
use crate::vsock::chroot_into_rootfs;

/// Execute the OCI entrypoint process.
#[cfg(target_os = "linux")]
pub fn run_entrypoint(process: OciProcess) {
    if process.args.is_empty() {
        log_error!("process.args is empty — no entrypoint to execute");
        reaper_loop();
    }

    let program = &process.args[0];
    let args = &process.args[1..];

    log_info!("executing entrypoint: {} {}", program, args.join(" "));

    // Set working directory
    if let Some(ref cwd) = process.cwd {
        if Path::new(cwd).exists() {
            if let Err(e) = std::env::set_current_dir(cwd) {
                log_error!("failed to chdir to '{}': {}", cwd, e);
            }
        }
    }

    // Set environment variables from OCI spec
    for env_entry in &process.env {
        if let Some((key, value)) = env_entry.split_once('=') {
            std::env::set_var(key, value);
        }
    }

    // Spawn the entrypoint. In initrd mode chroot into the virtiofs-mounted rootfs
    // first; in no-initrd mode we are already running inside the container rootfs.
    // PID 1 stays alive to continue reaping orphaned zombies.
    match unsafe {
        Command::new(program)
            .args(args)
            .pre_exec(|| {
                if is_initrd_mode() {
                    chroot_into_rootfs()
                } else {
                    Ok(())
                }
            })
            .spawn()
    } {
        Ok(mut child) => {
            let child_pid = child.id();
            log_info!("entrypoint spawned (pid={})", child_pid);

            // Poll loop: wait for entrypoint while reaping zombies
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        let code = status.code().unwrap_or(1);
                        log_info!("entrypoint exited with code {}", code);
                        reap_zombies();
                        shutdown(code);
                        return;
                    }
                    Ok(None) => {
                        reap_zombies();
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                    Err(e) => {
                        log_error!("wait error: {}", e);
                        shutdown(1);
                        return;
                    }
                }
            }
        }
        Err(e) => {
            log_error!("failed to spawn entrypoint '{}': {}", program, e);
            reaper_loop();
        }
    }
}

/// Graceful shutdown — sends SIGTERM, then SIGKILL, unmounts, sync, power off.
#[cfg(target_os = "linux")]
pub fn shutdown(exit_code: i32) {
    use std::ffi::CString;

    log_info!("shutting down (code={})", exit_code);

    // SIGTERM → all
    unsafe { libc::kill(-1, libc::SIGTERM) };
    std::thread::sleep(std::time::Duration::from_secs(2));

    // SIGKILL → all
    unsafe { libc::kill(-1, libc::SIGKILL) };
    reap_zombies();

    // Unmount pseudo-filesystems (best-effort, reverse order)
    let unmounts = [
        "/run", "/tmp", "/dev/shm", "/dev/pts", "/dev", "/sys", "/proc",
    ];
    for target in &unmounts {
        let c_target = match CString::new(*target) {
            Ok(c) => c,
            Err(_) => continue,
        };
        unsafe {
            libc::umount2(c_target.as_ptr(), libc::MNT_DETACH);
        }
    }

    // Sync filesystems
    unsafe { libc::sync() };

    // Power off the VM
    unsafe { libc::reboot(libc::LINUX_REBOOT_CMD_POWER_OFF) };

    // Unreachable, but satisfy the compiler
    std::process::exit(exit_code);
}
