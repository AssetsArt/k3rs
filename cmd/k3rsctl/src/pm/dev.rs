//! `pm dev` — run k3rs components in dev mode.
//!
//! - Server/Agent/Vpc: `cargo watch -x "run --bin <bin> -- <args>"`
//! - UI: `dx serve --package k3rs-ui`
//!
//! Press Ctrl+C to stop everything.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Result, bail};

use super::types::ComponentName;

// ── Dev config ──────────────────────────────────────────────────

struct DevConfig {
    label: &'static str,
    bin_name: &'static str,
    args: Vec<String>,
    watch_dirs: Vec<&'static str>,
    env: HashMap<String, String>,
    #[allow(dead_code)]
    url: &'static str,
    #[allow(dead_code)]
    color_idx: usize,
    ports: Vec<u16>,
}

fn server_config() -> DevConfig {
    DevConfig {
        label: "server",
        bin_name: "k3rs-server",
        args: [
            "--port",
            &pkg_constants::network::DEFAULT_API_PORT.to_string(),
            "--token",
            pkg_constants::auth::DEFAULT_JOIN_TOKEN,
            "--data-dir",
            pkg_constants::paths::DATA_DIR,
            "--node-name",
            "master-1",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
        watch_dirs: vec!["pkg/", "cmd/k3rs-server"],
        env: [("RUST_LOG".into(), "debug".into())].into(),
        url: "http://127.0.0.1:6443",
        color_idx: 0,
        ports: vec![pkg_constants::network::DEFAULT_API_PORT],
    }
}

fn agent_config() -> DevConfig {
    DevConfig {
        label: "agent",
        bin_name: "k3rs-agent",
        args: [
            "--server",
            pkg_constants::network::DEFAULT_API_ADDR,
            "--token",
            pkg_constants::auth::DEFAULT_JOIN_TOKEN,
            "--node-name",
            "node-1",
            "--proxy-port",
            &pkg_constants::network::DEFAULT_TUNNEL_PORT.to_string(),
            "--service-proxy-port",
            &pkg_constants::network::DEFAULT_SERVICE_PROXY_PORT.to_string(),
            "--dns-port",
            &pkg_constants::network::DEFAULT_DNS_PORT.to_string(),
            "--vpc-socket",
            pkg_constants::paths::VPC_SOCKET,
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
        watch_dirs: vec!["pkg/", "cmd/k3rs-agent"],
        env: [("RUST_LOG".into(), "debug".into())].into(),
        url: "proxy :6444 | svc :10256 | dns :5353",
        color_idx: 1,
        ports: vec![
            pkg_constants::network::DEFAULT_TUNNEL_PORT,
            pkg_constants::network::DEFAULT_SERVICE_PROXY_PORT,
            pkg_constants::network::DEFAULT_DNS_PORT,
        ],
    }
}

fn vpc_config() -> DevConfig {
    let vpc_socket: &'static str =
        format!("{}/k3rs-vpc.sock", pkg_constants::paths::DATA_DIR).leak();
    DevConfig {
        label: "vpc",
        bin_name: "k3rs-vpc",
        args: [
            "--server-url",
            pkg_constants::network::DEFAULT_API_ADDR,
            "--token",
            pkg_constants::auth::DEFAULT_JOIN_TOKEN,
            "--socket",
            pkg_constants::paths::VPC_SOCKET,
            "--data-dir",
            &format!("{}/vpc-state.db", pkg_constants::paths::DATA_DIR),
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
        watch_dirs: vec!["pkg/", "cmd/k3rs-vpc"],
        env: [("RUST_LOG".into(), "debug".into())].into(),
        url: vpc_socket,
        color_idx: 2,
        ports: vec![],
    }
}

fn ui_config() -> DevConfig {
    DevConfig {
        label: "ui",
        bin_name: "",
        args: vec![],
        watch_dirs: vec![],
        env: HashMap::new(),
        url: "http://127.0.0.1:8080",
        color_idx: 3,
        ports: vec![8080],
    }
}

// ── Kill existing port holders ──────────────────────────────────

fn kill_ports(configs: &[DevConfig]) {
    for config in configs {
        for &port in &config.ports {
            if let Ok(output) = Command::new("lsof")
                .args(["-ti", &format!(":{}", port), "-sTCP:LISTEN"])
                .output()
            {
                let stdout = String::from_utf8_lossy(&output.stdout);
                for pid_str in stdout.split_whitespace() {
                    if let Ok(pid) = pid_str.parse::<u32>() {
                        if pid == std::process::id() {
                            continue;
                        }
                        let _ = Command::new("kill").arg("-9").arg(pid_str).status();
                    }
                }
            }
        }
    }
}

// ── Process spawning ────────────────────────────────────────────

fn spawn_component(config: &DevConfig, running: Arc<AtomicBool>) -> Result<std::process::Child> {
    let is_ui = config.bin_name.is_empty();

    let mut child = if is_ui {
        Command::new("dx")
            .args(["serve", "--package", "k3rs-ui"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?
    } else {
        let mut run_cmd = format!("run -p {} --bin {} --", config.bin_name, config.bin_name);
        for arg in &config.args {
            run_cmd.push(' ');
            run_cmd.push_str(arg);
        }

        let mut cmd = Command::new("cargo");
        cmd.arg("watch").arg("-x").arg(&run_cmd);
        for dir in &config.watch_dirs {
            cmd.arg("-w").arg(dir);
        }
        cmd.arg("-i").arg("target/*");
        for (k, v) in &config.env {
            cmd.env(k, v);
        }
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?
    };

    let label = config.label.to_string();

    // stdout reader
    if let Some(stdout) = child.stdout.take() {
        let running = Arc::clone(&running);
        let label = label.clone();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                if !running.load(Ordering::Relaxed) {
                    break;
                }
                println!("[{}] {}", label, line);
            }
        });
    }

    // stderr reader
    if let Some(stderr) = child.stderr.take() {
        let running = Arc::clone(&running);
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                if !running.load(Ordering::Relaxed) {
                    break;
                }
                eprintln!("[{}] {}", label, line);
            }
        });
    }

    Ok(child)
}

// ── Tool check ──────────────────────────────────────────────────

fn check_tools(components: &[ComponentName]) -> Result<()> {
    let needs_cargo_watch = components.iter().any(|c| {
        matches!(
            c,
            ComponentName::Server | ComponentName::Agent | ComponentName::Vpc
        )
    });
    let needs_dx = components.iter().any(|c| matches!(c, ComponentName::Ui));

    if needs_cargo_watch {
        let s = Command::new("cargo")
            .arg("watch")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if s.is_err() || !s.unwrap().success() {
            bail!("cargo-watch not installed. Run: cargo install cargo-watch");
        }
    }
    if needs_dx {
        let s = Command::new("dx")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if s.is_err() || !s.unwrap().success() {
            bail!("dioxus-cli (dx) not installed. Run: cargo install dioxus-cli");
        }
    }
    Ok(())
}

// ── Main entry ──────────────────────────────────────────────────

pub fn run(component: &ComponentName) -> Result<()> {
    let components = component.resolve();
    check_tools(&components)?;

    let configs: Vec<DevConfig> = components
        .iter()
        .map(|c| match c {
            ComponentName::Server => server_config(),
            ComponentName::Agent => agent_config(),
            ComponentName::Vpc => vpc_config(),
            ComponentName::Ui => ui_config(),
            ComponentName::All => unreachable!(),
        })
        .collect();

    // Kill any processes already using our ports
    kill_ports(&configs);

    let running = Arc::new(AtomicBool::new(true));

    // Stop cleanly on Ctrl-C
    let r = Arc::clone(&running);
    ctrlc::set_handler(move || {
        println!("\nReceived Ctrl-C, shutting down...");
        r.store(false, Ordering::Relaxed);
    })
    .expect("Error setting Ctrl-C handler");

    // Spawn all components
    let mut children: Vec<std::process::Child> = Vec::new();
    for config in configs.iter() {
        let child = spawn_component(config, Arc::clone(&running))?;
        children.push(child);
    }

    println!("All components started. Logs will appear below (Ctrl+C to quit)...");

    // Wait until running flag is flipped (due to SigInt)
    while running.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    // Shutdown
    println!("Stopping processes...");
    for child in children.iter_mut() {
        let _ = child.kill();
        let _ = child.wait();
    }

    Ok(())
}
