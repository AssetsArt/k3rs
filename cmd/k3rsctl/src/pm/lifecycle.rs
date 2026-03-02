use std::fs;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;

use super::{registry, watchdog};
use super::types::{ComponentName, ProcessStatus};

/// Start one or more components as background daemons.
pub fn start(component: &ComponentName, foreground: bool) -> Result<()> {
    registry::ensure_dirs()?;

    for comp in component.resolve() {
        start_one(&comp, foreground)?;
    }
    Ok(())
}

fn start_one(component: &ComponentName, foreground: bool) -> Result<()> {
    let key = component.key().to_string();
    let reg = registry::load()?;

    let entry = reg
        .processes
        .get(&key)
        .with_context(|| format!("{} is not installed. Run `k3rsctl pm install {}` first.", key, key))?;

    // Check if already running
    if let Some(pid) = entry.pid {
        if is_alive(pid) {
            println!("{} is already running (pid {})", key, pid);
            return Ok(());
        }
    }

    let bin_path = &entry.bin_path;
    if !bin_path.exists() {
        bail!(
            "binary not found at {}. Re-install with `k3rsctl pm install {}`.",
            bin_path.display(),
            key
        );
    }

    // Build CLI args from config YAML and persist to registry
    if let Some(config_path) = &entry.config_path {
        if config_path.exists() && entry.args.is_empty() {
            let args = build_args_from_config(config_path)?;
            let config_p = config_path.clone();
            let k = key.clone();
            registry::update(|reg| {
                if let Some(e) = reg.processes.get_mut(&k) {
                    e.args = args;
                    // Also pass --config to the binary
                    e.args.insert(0, config_p.display().to_string());
                    e.args.insert(0, "--config".to_string());
                }
            })?;
        }
    }
    // Re-load after potential update
    let reg = registry::load()?;
    let entry = reg.processes.get(&key).unwrap();

    let stdout_log = &entry.stdout_log;
    let stderr_log = &entry.stderr_log;

    println!("Starting {}...", key);

    if foreground {
        // Run in foreground — blocks until the process exits
        let status = Command::new(bin_path)
            .args(&entry.args)
            .envs(&entry.env)
            .status()
            .with_context(|| format!("failed to start {}", key))?;

        println!("{} exited with status {:?}", key, status.code());
        return Ok(());
    }

    if entry.auto_restart {
        // Spawn watchdog sidecar — it handles spawning + monitoring the component
        watchdog::spawn_watchdog(component)?;

        // Wait for watchdog to spawn the component and write the PID
        thread::sleep(Duration::from_secs(2));

        let reg = registry::load()?;
        if let Some(e) = reg.processes.get(&key) {
            if let Some(pid) = e.pid {
                if is_alive(pid) {
                    println!("  {} started (pid {}, watchdog active)", key, pid);
                } else {
                    eprintln!(
                        "  Warning: {} exited within 2s — check logs at {}",
                        key,
                        stderr_log.display()
                    );
                }
            }
        }
    } else {
        // Direct daemonized spawn without watchdog
        let stdout_file = fs::File::create(stdout_log)
            .with_context(|| format!("failed to create {}", stdout_log.display()))?;
        let stderr_file = fs::File::create(stderr_log)
            .with_context(|| format!("failed to create {}", stderr_log.display()))?;

        let child = unsafe {
            Command::new(bin_path)
                .args(&entry.args)
                .envs(&entry.env)
                .stdout(stdout_file)
                .stderr(stderr_file)
                .pre_exec(|| {
                    nix::unistd::setsid()
                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
                    Ok(())
                })
                .spawn()
                .with_context(|| format!("failed to spawn {}", key))?
        };

        let pid = child.id();

        // Write PID file
        let pid_path = registry::pids_dir().join(format!("{}.pid", key));
        fs::write(&pid_path, pid.to_string())
            .with_context(|| format!("failed to write PID file {}", pid_path.display()))?;

        // Update registry
        registry::update(|reg| {
            if let Some(entry) = reg.processes.get_mut(&key) {
                entry.pid = Some(pid);
                entry.status = ProcessStatus::Running;
                entry.started_at = Some(Utc::now());
            }
        })?;

        // Wait briefly and verify the process is still alive
        thread::sleep(Duration::from_secs(2));
        if is_alive(pid) {
            println!("  {} started (pid {})", key, pid);
        } else {
            eprintln!(
                "  Warning: {} (pid {}) exited within 2s — check logs at {}",
                key,
                pid,
                stderr_log.display()
            );
            registry::update(|reg| {
                if let Some(entry) = reg.processes.get_mut(&key) {
                    entry.status = ProcessStatus::Crashed;
                    entry.pid = None;
                }
            })?;
        }
    }

    Ok(())
}

/// Stop one or more components.
pub fn stop(component: &ComponentName, force: bool, timeout_secs: u64) -> Result<()> {
    for comp in component.resolve() {
        stop_one(&comp, force, timeout_secs)?;
    }
    Ok(())
}

fn stop_one(component: &ComponentName, force: bool, timeout_secs: u64) -> Result<()> {
    let key = component.key().to_string();
    let reg = registry::load()?;

    let entry = reg
        .processes
        .get(&key)
        .with_context(|| format!("{} is not registered", key))?;

    let pid = match entry.pid {
        Some(p) if is_alive(p) => p,
        _ => {
            println!("{} is not running", key);
            // Still clean up state if needed
            registry::update(|reg| {
                if let Some(e) = reg.processes.get_mut(&key) {
                    e.status = ProcessStatus::Stopped;
                    e.pid = None;
                }
            })?;
            return Ok(());
        }
    };

    // Kill watchdog first so it doesn't restart the component
    watchdog::stop_watchdog(&key);

    println!("Stopping {} (pid {})...", key, pid);
    let nix_pid = Pid::from_raw(pid as i32);

    if force {
        signal::kill(nix_pid, Signal::SIGKILL).ok();
    } else {
        // Graceful: SIGTERM first
        signal::kill(nix_pid, Signal::SIGTERM).ok();

        let deadline = Duration::from_secs(timeout_secs);
        let poll_interval = Duration::from_millis(200);
        let start = std::time::Instant::now();

        while start.elapsed() < deadline {
            if !is_alive(pid) {
                break;
            }
            thread::sleep(poll_interval);
        }

        // Escalate to SIGKILL if still alive
        if is_alive(pid) {
            eprintln!("  {} did not stop within {}s, sending SIGKILL", key, timeout_secs);
            signal::kill(nix_pid, Signal::SIGKILL).ok();
            thread::sleep(Duration::from_millis(500));
        }
    }

    // Remove PID file
    let pid_path = registry::pids_dir().join(format!("{}.pid", key));
    let _ = fs::remove_file(&pid_path);

    // Update registry
    registry::update(|reg| {
        if let Some(e) = reg.processes.get_mut(&key) {
            e.status = ProcessStatus::Stopped;
            e.pid = None;
            e.started_at = None;
        }
    })?;

    println!("  {} stopped", key);
    Ok(())
}

/// Restart: stop then start.
pub fn restart(component: &ComponentName, force: bool, timeout_secs: u64) -> Result<()> {
    stop(component, force, timeout_secs)?;
    start(component, false)
}

/// Delete one or more components: stop if running, remove from registry, cleanup files.
pub fn delete(
    component: &ComponentName,
    keep_data: bool,
    keep_binary: bool,
    keep_logs: bool,
) -> Result<()> {
    for comp in component.resolve() {
        delete_one(&comp, keep_data, keep_binary, keep_logs)?;
    }
    Ok(())
}

fn delete_one(
    component: &ComponentName,
    keep_data: bool,
    keep_binary: bool,
    keep_logs: bool,
) -> Result<()> {
    let key = component.key().to_string();
    let reg = registry::load()?;

    let entry = match reg.processes.get(&key) {
        Some(e) => e.clone(),
        None => {
            println!("{} is not registered, nothing to delete", key);
            return Ok(());
        }
    };

    // Stop if running
    if entry.pid.is_some_and(|p| is_alive(p)) {
        stop_one(component, false, 10)?;
    }

    // Remove PID file
    let pid_path = registry::pids_dir().join(format!("{}.pid", key));
    let _ = fs::remove_file(&pid_path);

    // Remove config
    if let Some(config_path) = &entry.config_path {
        let _ = fs::remove_file(config_path);
    }

    // Conditional cleanup
    if !keep_binary {
        let _ = fs::remove_file(&entry.bin_path);
    }

    if !keep_logs {
        let _ = fs::remove_file(&entry.stdout_log);
        let _ = fs::remove_file(&entry.stderr_log);
    }

    if !keep_data {
        let data_dir = dirs::home_dir()
            .expect("could not determine home directory")
            .join(".k3rs")
            .join("data")
            .join(&key);
        if data_dir.exists() {
            let _ = fs::remove_dir_all(&data_dir);
        }
    }

    // Remove from registry
    registry::update(|reg| {
        reg.processes.remove(&key);
    })?;

    println!("Deleted {}", key);
    Ok(())
}

/// Read a YAML config file and convert key-value pairs to `--key value` CLI args.
/// Skips comment-only lines. Only handles flat key: value (no nested maps).
fn build_args_from_config(config_path: &std::path::Path) -> Result<Vec<String>> {
    let content = fs::read_to_string(config_path)
        .with_context(|| format!("failed to read config {}", config_path.display()))?;

    let mut args = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((key, val)) = trimmed.split_once(':') {
            let key = key.trim();
            let val = val.trim();
            if !val.is_empty() {
                args.push(format!("--{}", key));
                args.push(val.to_string());
            }
        }
    }
    Ok(args)
}

/// Check if a process with the given PID is alive using signal 0.
pub fn is_alive(pid: u32) -> bool {
    signal::kill(Pid::from_raw(pid as i32), None).is_ok()
}
