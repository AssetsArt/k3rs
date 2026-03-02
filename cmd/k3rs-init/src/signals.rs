use std::io::Write;

/// Install signal handlers for PID 1.
///
/// We do NOT set SIGCHLD=SIG_IGN here, because that breaks Command::output()
/// (waitpid returns ECHILD immediately when SIGCHLD=SIG_IGN).
/// Instead, reap_zombies() is called periodically with WNOHANG.
#[cfg(target_os = "linux")]
pub fn install_signal_handlers() {
    log_info!("signal handlers installed");
}

/// Reap all finished child processes (non-blocking).
#[cfg(target_os = "linux")]
pub fn reap_zombies() {
    use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
    use nix::unistd::Pid;

    loop {
        match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => break,
            Ok(WaitStatus::Exited(pid, code)) => {
                log_info!("reaped zombie pid={} exit={}", pid, code);
            }
            Ok(WaitStatus::Signaled(pid, sig, _)) => {
                log_info!("reaped zombie pid={} signal={}", pid, sig);
            }
            Ok(_) => continue,
            Err(nix::errno::Errno::ECHILD) => break, // No children
            Err(_) => break,
        }
    }
}

/// Reaper-only loop — used when no entrypoint is configured.
/// PID 1 must **never** exit, so we loop forever.
#[cfg(target_os = "linux")]
pub fn reaper_loop() -> ! {
    log_info!("entering reaper-only mode (no entrypoint)");
    loop {
        reap_zombies();
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
