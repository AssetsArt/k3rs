use anyhow::Result;
use chrono::Utc;
use sysinfo::System;

use super::lifecycle::is_alive;
use super::registry;
use super::types::ProcessStatus;

pub fn list() -> Result<()> {
    let mut reg = registry::load()?;

    if reg.processes.is_empty() {
        println!("No components registered. Run `k3rsctl pm install <component>` first.");
        return Ok(());
    }

    // Refresh liveness for each entry
    for entry in reg.processes.values_mut() {
        if let Some(pid) = entry.pid {
            if !is_alive(pid) {
                entry.status = if entry.status == ProcessStatus::Running {
                    ProcessStatus::Crashed
                } else {
                    ProcessStatus::Stopped
                };
                entry.pid = None;
            }
        }
    }
    // Persist refreshed state
    registry::save(&reg)?;

    // Collect CPU/memory for live PIDs
    let mut sys = System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

    // Header
    println!(
        "\x1b[1m{:<12} {:<12} {:<8} {:<8} {:<10} {:<10} {:<10}\x1b[0m",
        "Name", "Status", "PID", "CPU%", "Mem", "Uptime", "Restarts"
    );
    println!("{}", "-".repeat(70));

    // Sort by name for deterministic output
    let mut keys: Vec<_> = reg.processes.keys().cloned().collect();
    keys.sort();

    for key in &keys {
        let entry = &reg.processes[key];

        let (status_icon, color) = match entry.status {
            ProcessStatus::Running => ("\u{25cf}", "\x1b[32m"),   // green
            ProcessStatus::Stopped => ("\u{25cb}", "\x1b[90m"),   // gray
            ProcessStatus::Crashed => ("\u{2715}", "\x1b[31m"),   // red
            ProcessStatus::Installing => ("\u{27f3}", "\x1b[33m"), // yellow
            ProcessStatus::Errored => ("\u{2715}", "\x1b[31m"),   // red
        };

        let pid_str = entry
            .pid
            .map(|p| p.to_string())
            .unwrap_or_else(|| "-".to_string());

        let (cpu_str, mem_str) = entry
            .pid
            .and_then(|pid| {
                let sysinfo_pid = sysinfo::Pid::from_u32(pid);
                sys.process(sysinfo_pid).map(|proc_info| {
                    let cpu = format!("{:.1}", proc_info.cpu_usage());
                    let mem_bytes = proc_info.memory();
                    let mem = if mem_bytes >= 1024 * 1024 * 1024 {
                        format!("{:.1}G", mem_bytes as f64 / (1024.0 * 1024.0 * 1024.0))
                    } else if mem_bytes >= 1024 * 1024 {
                        format!("{:.1}M", mem_bytes as f64 / (1024.0 * 1024.0))
                    } else {
                        format!("{}K", mem_bytes / 1024)
                    };
                    (cpu, mem)
                })
            })
            .unwrap_or_else(|| ("-".to_string(), "-".to_string()));

        let uptime_str = entry
            .started_at
            .filter(|_| entry.status == ProcessStatus::Running)
            .map(|started| {
                let dur = Utc::now().signed_duration_since(started);
                let secs = dur.num_seconds();
                if secs < 60 {
                    format!("{}s", secs)
                } else if secs < 3600 {
                    format!("{}m", secs / 60)
                } else if secs < 86400 {
                    format!("{}h", secs / 3600)
                } else {
                    format!("{}d", secs / 86400)
                }
            })
            .unwrap_or_else(|| "-".to_string());

        println!(
            "{}{} {:<10}\x1b[0m {:<12} {:<8} {:<8} {:<10} {:<10} {:<10}",
            color,
            status_icon,
            entry.name,
            format!("{}", entry.status),
            pid_str,
            cpu_str,
            mem_str,
            uptime_str,
            entry.restart_count,
        );
    }

    Ok(())
}
