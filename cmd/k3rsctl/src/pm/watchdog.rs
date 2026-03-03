use std::fs;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;

use super::lifecycle::is_alive;
use super::registry;
use super::types::{ComponentName, ProcessStatus};

/// Run the watchdog supervisor loop for a component.
/// This is invoked as a detached sidecar via `k3rsctl pm _watch <component>`.
/// It spawns the component, monitors it, and restarts on crash with exponential backoff.
pub fn run(component: &ComponentName) -> Result<()> {
    let key = component.key().to_string();

    // Write watchdog PID file
    let watch_pid_path = registry::pids_dir().join(format!("{}-watch.pid", key));
    fs::write(&watch_pid_path, std::process::id().to_string()).with_context(|| {
        format!(
            "failed to write watchdog PID file {}",
            watch_pid_path.display()
        )
    })?;

    loop {
        let reg = registry::load()?;
        let entry = match reg.processes.get(&key) {
            Some(e) => e.clone(),
            None => {
                // Component was deleted from registry — exit watchdog
                let _ = fs::remove_file(&watch_pid_path);
                return Ok(());
            }
        };

        // Spawn the component
        let pid = spawn_component(&key, &entry)?;

        // Update registry with new PID
        registry::update(|reg| {
            if let Some(e) = reg.processes.get_mut(&key) {
                e.pid = Some(pid);
                e.status = ProcessStatus::Running;
                e.started_at = Some(Utc::now());
            }
        })?;

        // Monitor loop: poll every 2s
        loop {
            thread::sleep(Duration::from_secs(2));

            if !is_alive(pid) {
                break; // Process died — handle restart below
            }

            // Check if we've been removed from registry (pm delete / pm stop)
            let reg = registry::load()?;
            match reg.processes.get(&key) {
                Some(e) if e.status == ProcessStatus::Stopped => {
                    // Explicitly stopped — exit watchdog
                    let _ = fs::remove_file(&watch_pid_path);
                    return Ok(());
                }
                None => {
                    // Deleted — exit watchdog
                    let _ = fs::remove_file(&watch_pid_path);
                    return Ok(());
                }
                _ => {}
            }
        }

        // Process crashed — decide whether to restart
        let reg = registry::load()?;
        let entry = match reg.processes.get(&key) {
            Some(e) => e.clone(),
            None => {
                let _ = fs::remove_file(&watch_pid_path);
                return Ok(());
            }
        };

        // If status was set to Stopped (by pm stop), exit cleanly
        if entry.status == ProcessStatus::Stopped {
            let _ = fs::remove_file(&watch_pid_path);
            return Ok(());
        }

        if !entry.auto_restart {
            registry::update(|reg| {
                if let Some(e) = reg.processes.get_mut(&key) {
                    e.status = ProcessStatus::Crashed;
                    e.pid = None;
                }
            })?;
            let _ = fs::remove_file(&watch_pid_path);
            return Ok(());
        }

        let restart_count = entry.restart_count;

        // Check max_restarts (0 = unlimited)
        if entry.max_restarts > 0 && restart_count >= entry.max_restarts {
            registry::update(|reg| {
                if let Some(e) = reg.processes.get_mut(&key) {
                    e.status = ProcessStatus::Crashed;
                    e.pid = None;
                }
            })?;
            let _ = fs::remove_file(&watch_pid_path);
            return Ok(());
        }

        // Exponential backoff: 1s, 2s, 4s, 8s, ... capped at 30s
        let delay_secs = std::cmp::min(30, 1u64 << restart_count.min(4));
        thread::sleep(Duration::from_secs(delay_secs));

        // Increment restart count
        registry::update(|reg| {
            if let Some(e) = reg.processes.get_mut(&key) {
                e.restart_count = restart_count + 1;
                e.status = ProcessStatus::Running;
            }
        })?;

        // Loop back to spawn again
    }
}

/// Spawn the component process (detached with log redirection).
/// Returns the child PID.
fn spawn_component(key: &str, entry: &super::types::ProcessEntry) -> Result<u32> {
    let stdout_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&entry.stdout_log)
        .with_context(|| format!("failed to open {}", entry.stdout_log.display()))?;
    let stderr_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&entry.stderr_log)
        .with_context(|| format!("failed to open {}", entry.stderr_log.display()))?;

    let child = Command::new(&entry.bin_path)
        .args(&entry.args)
        .envs(&entry.env)
        .stdout(stdout_file)
        .stderr(stderr_file)
        .spawn()
        .with_context(|| format!("failed to spawn {}", key))?;

    let pid = child.id();

    // Write component PID file
    let pid_path = registry::pids_dir().join(format!("{}.pid", key));
    fs::write(&pid_path, pid.to_string())
        .with_context(|| format!("failed to write PID file {}", pid_path.display()))?;

    Ok(pid)
}

/// Spawn the watchdog sidecar as a detached process.
/// Called from `lifecycle::start_one` when `auto_restart` is true.
pub fn spawn_watchdog(component: &ComponentName) -> Result<()> {
    let key = component.key();

    // Get path to current executable (k3rsctl)
    let exe = std::env::current_exe().context("failed to determine current executable path")?;

    let watch_log = registry::logs_dir().join(format!("{}-watch.log", key));
    let log_file = fs::File::create(&watch_log)
        .with_context(|| format!("failed to create {}", watch_log.display()))?;

    let _child = unsafe {
        Command::new(&exe)
            .args(["pm", "_watch", key])
            .stdout(log_file.try_clone()?)
            .stderr(log_file)
            .pre_exec(|| {
                nix::unistd::setsid().map_err(std::io::Error::other)?;
                Ok(())
            })
            .spawn()
            .with_context(|| format!("failed to spawn watchdog for {}", key))?
    };

    Ok(())
}

/// Kill the watchdog sidecar for a component (if running).
pub fn stop_watchdog(key: &str) {
    let watch_pid_path = registry::pids_dir().join(format!("{}-watch.pid", key));
    if let Ok(content) = fs::read_to_string(&watch_pid_path)
        && let Ok(pid) = content.trim().parse::<u32>()
        && is_alive(pid)
    {
        let nix_pid = nix::unistd::Pid::from_raw(pid as i32);
        let _ = nix::sys::signal::kill(nix_pid, nix::sys::signal::Signal::SIGTERM);
        // Give it a moment to clean up
        thread::sleep(Duration::from_millis(500));
        if is_alive(pid) {
            let _ = nix::sys::signal::kill(nix_pid, nix::sys::signal::Signal::SIGKILL);
        }
    }
    let _ = fs::remove_file(&watch_pid_path);
}
