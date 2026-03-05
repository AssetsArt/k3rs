use std::fs;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};

use super::registry;

/// Generate systemd unit files for all registered components.
pub fn startup(user: bool, enable: bool) -> Result<()> {
    let reg = registry::load()?;

    if reg.processes.is_empty() {
        println!("No components registered. Nothing to generate.");
        return Ok(());
    }

    let output_dir = if user {
        dirs::home_dir()
            .expect("could not determine home directory")
            .join(".config/systemd/user")
    } else {
        PathBuf::from("/etc/systemd/system")
    };

    fs::create_dir_all(&output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;

    let mut generated = Vec::new();

    for (key, entry) in &reg.processes {
        let service_name = format!("k3rs-{}", key);
        let unit_path = output_dir.join(format!("{}.service", service_name));

        let description = match key.as_str() {
            "server" => "K3rs Control Plane Server",
            "agent" => "K3rs Agent (Data Plane)",
            "vpc" => "K3rs VPC Daemon",
            "ui" => "K3rs Management Dashboard",
            _ => "K3rs Component",
        };

        let wanted_by = if user {
            "default.target"
        } else {
            "multi-user.target"
        };

        let mut exec_start = entry.bin_path.display().to_string();
        if let Some(config_path) = &entry.config_path {
            exec_start.push_str(&format!(" \\\n  --config {}", config_path.display()));
        }
        for arg in &entry.args {
            exec_start.push_str(&format!(" \\\n  {}", arg));
        }

        // Build capability directives (only for system-level services)
        let caps = pkg_constants::capabilities::caps_for_component(key);
        let caps_section = if !user && !caps.is_empty() {
            let caps_str = caps.join(" ");
            format!(
                "AmbientCapabilities={caps}\n\
                 CapabilityBoundingSet={caps}\n\
                 NoNewPrivileges=true\n",
                caps = caps_str,
            )
        } else {
            String::new()
        };

        let unit = format!(
            "\
[Unit]
Description={description}
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart={exec_start}
Restart=on-failure
RestartSec={restart_sec}
{caps_section}\
StandardOutput=append:{stdout}
StandardError=append:{stderr}

[Install]
WantedBy={wanted_by}
",
            description = description,
            exec_start = exec_start,
            restart_sec = pkg_constants::timings::SYSTEMD_RESTART_SECS,
            caps_section = caps_section,
            stdout = entry.stdout_log.display(),
            stderr = entry.stderr_log.display(),
            wanted_by = wanted_by,
        );

        fs::write(&unit_path, &unit)
            .with_context(|| format!("failed to write {}", unit_path.display()))?;

        println!("Generated {}", unit_path.display());
        generated.push(service_name);
    }

    if enable && !generated.is_empty() {
        let mut cmd = Command::new("systemctl");
        if user {
            cmd.arg("--user");
        }
        cmd.arg("enable");
        for name in &generated {
            cmd.arg(format!("{}.service", name));
        }

        let status = cmd.status().context("failed to run systemctl enable")?;
        if status.success() {
            println!("Enabled {} service(s)", generated.len());
        } else {
            eprintln!("systemctl enable exited with {:?}", status.code());
        }
    }

    if !enable {
        let flag = if user { " --user" } else { "" };
        println!(
            "\nTo enable on boot: systemctl{} enable {}",
            flag,
            generated
                .iter()
                .map(|s| format!("{}.service", s))
                .collect::<Vec<_>>()
                .join(" ")
        );
    }

    Ok(())
}
