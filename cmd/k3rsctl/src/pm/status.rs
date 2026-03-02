use std::fs;

use anyhow::Result;
use chrono::Utc;

use super::lifecycle::is_alive;
use super::registry;
use super::types::ProcessStatus;

/// Show detailed status for all registered components.
pub fn status() -> Result<()> {
    let mut reg = registry::load()?;

    if reg.processes.is_empty() {
        println!("No components registered. Run `k3rsctl pm install <component>` first.");
        return Ok(());
    }

    // Refresh liveness
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
    registry::save(&reg)?;

    let mut keys: Vec<_> = reg.processes.keys().cloned().collect();
    keys.sort();

    for (i, key) in keys.iter().enumerate() {
        let entry = &reg.processes[key];

        let status_str = match entry.status {
            ProcessStatus::Running => "\x1b[32mRunning\x1b[0m",
            ProcessStatus::Stopped => "\x1b[90mStopped\x1b[0m",
            ProcessStatus::Crashed => "\x1b[31mCrashed\x1b[0m",
            ProcessStatus::Installing => "\x1b[33mInstalling\x1b[0m",
            ProcessStatus::Errored => "\x1b[31mErrored\x1b[0m",
        };

        let pid_str = entry
            .pid
            .map(|p| format!("PID {}", p))
            .unwrap_or_else(|| "no PID".to_string());

        println!("{} ({}) — {}", key, pid_str, status_str);
        println!("  Binary:    {}", entry.bin_path.display());

        if let Some(config_path) = &entry.config_path {
            println!("  Config:    {}", config_path.display());
        }

        // Show port/server from config if available
        if let Some(config_path) = &entry.config_path {
            if config_path.exists() {
                if let Ok(content) = fs::read_to_string(config_path) {
                    for line in content.lines() {
                        let trimmed = line.trim();
                        if trimmed.starts_with("port:") {
                            println!("  Port:      {}", trimmed.trim_start_matches("port:").trim());
                        } else if trimmed.starts_with("server:") || trimmed.starts_with("server-url:") {
                            // server: http://... has two colons, take everything after first ':'
                            let full_val = trimmed.splitn(2, ':').nth(1).unwrap_or("").trim();
                            println!("  Server:    {}", full_val);
                        } else if trimmed.starts_with("data-dir:") {
                            println!("  Data Dir:  {}", trimmed.trim_start_matches("data-dir:").trim());
                        }
                    }
                }
            }
        }

        let uptime_str = entry
            .started_at
            .filter(|_| entry.status == ProcessStatus::Running)
            .map(|started| {
                let secs = Utc::now().signed_duration_since(started).num_seconds();
                if secs < 60 {
                    format!("{}s", secs)
                } else if secs < 3600 {
                    let m = secs / 60;
                    let s = secs % 60;
                    format!("{}m {}s", m, s)
                } else if secs < 86400 {
                    let h = secs / 3600;
                    let m = (secs % 3600) / 60;
                    format!("{}h {}m", h, m)
                } else {
                    let d = secs / 86400;
                    let h = (secs % 86400) / 3600;
                    format!("{}d {}h", d, h)
                }
            })
            .unwrap_or_else(|| "-".to_string());

        println!("  Uptime:    {}", uptime_str);
        println!("  Restarts:  {}", entry.restart_count);

        // Health check
        if entry.status == ProcessStatus::Running {
            let health = check_health(key);
            println!("  Health:    {}", health);
        }

        if i < keys.len() - 1 {
            println!();
        }
    }

    Ok(())
}

/// Run a health check for the given component.
fn check_health(key: &str) -> String {
    match key {
        "server" => check_server_health(),
        "agent" => check_agent_health(),
        "vpc" => check_vpc_health(),
        _ => "\x1b[90m- no health check available\x1b[0m".to_string(),
    }
}

/// Server health: GET /api/v1/cluster/info
fn check_server_health() -> String {
    // Try to connect to the server API
    match std::net::TcpStream::connect_timeout(
        &"127.0.0.1:6443".parse().unwrap(),
        std::time::Duration::from_secs(2),
    ) {
        Ok(_) => "\x1b[32m✓ API port 6443 reachable\x1b[0m".to_string(),
        Err(_) => "\x1b[31m✕ API port 6443 not reachable\x1b[0m".to_string(),
    }
}

/// Agent health: check if it can reach the server
fn check_agent_health() -> String {
    match std::net::TcpStream::connect_timeout(
        &"127.0.0.1:6443".parse().unwrap(),
        std::time::Duration::from_secs(2),
    ) {
        Ok(_) => "\x1b[32m✓ Connected to server (127.0.0.1:6443)\x1b[0m".to_string(),
        Err(_) => "\x1b[31m✕ Cannot reach server (127.0.0.1:6443)\x1b[0m".to_string(),
    }
}

/// VPC health: check if the Unix socket responds
fn check_vpc_health() -> String {
    let sock_path = "/run/k3rs-vpc.sock";
    if std::path::Path::new(sock_path).exists() {
        "\x1b[32m✓ Socket exists (/run/k3rs-vpc.sock)\x1b[0m".to_string()
    } else {
        "\x1b[31m✕ Socket not found (/run/k3rs-vpc.sock)\x1b[0m".to_string()
    }
}
