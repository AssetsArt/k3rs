//! `pm dev` — run k3rs components in dev mode with ratatui TUI dashboard.
//!
//! - Server/Agent/Vpc: `cargo watch -x "run --bin <bin> -- <args>"`
//! - UI: `dx serve --package k3rs-ui`
//!
//! Press `q` or Ctrl+C to stop everything.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Result, bail};

use super::tui::{App, ComponentInfo, ComponentStatus, LogBuffer};
use super::types::ComponentName;

// ── Dev config ──────────────────────────────────────────────────

struct DevConfig {
    label: &'static str,
    bin_name: &'static str,
    args: Vec<String>,
    watch_dirs: Vec<&'static str>,
    env: HashMap<String, String>,
    url: &'static str,
    color_idx: usize,
    ports: Vec<u16>,
}

fn server_config() -> DevConfig {
    DevConfig {
        label: "server",
        bin_name: "k3rs-server",
        args: [
            "--port",
            "6443",
            "--token",
            "demo-token-123",
            "--data-dir",
            "/tmp/k3rs-data",
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
        ports: vec![6443],
    }
}

fn agent_config() -> DevConfig {
    DevConfig {
        label: "agent",
        bin_name: "k3rs-agent",
        args: [
            "--server",
            "http://127.0.0.1:6443",
            "--token",
            "demo-token-123",
            "--node-name",
            "node-1",
            "--proxy-port",
            "6444",
            "--service-proxy-port",
            "10256",
            "--dns-port",
            "5353",
            "--vpc-socket",
            "/tmp/k3rs-data/k3rs-vpc.sock",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
        watch_dirs: vec!["pkg/", "cmd/k3rs-agent"],
        env: [("RUST_LOG".into(), "debug".into())].into(),
        url: "proxy :6444 | svc :10256 | dns :5353",
        color_idx: 1,
        ports: vec![6444, 10256, 5353],
    }
}

fn vpc_config() -> DevConfig {
    DevConfig {
        label: "vpc",
        bin_name: "k3rs-vpc",
        args: [
            "--server-url",
            "http://127.0.0.1:6443",
            "--token",
            "demo-token-123",
            "--socket",
            "/tmp/k3rs-data/k3rs-vpc.sock",
            "--data-dir",
            "/tmp/k3rs-data/vpc-state.db",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
        watch_dirs: vec!["pkg/", "cmd/k3rs-vpc"],
        env: [("RUST_LOG".into(), "debug".into())].into(),
        url: "unix:///tmp/k3rs-data/k3rs-vpc.sock",
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

fn spawn_component(
    config: &DevConfig,
    running: Arc<AtomicBool>,
    buffer: LogBuffer,
    components: Arc<Mutex<Vec<ComponentInfo>>>,
    comp_idx: usize,
) -> Result<std::process::Child> {
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

    // Update PID
    {
        let mut comps = components.lock().unwrap();
        if let Some(comp) = comps.get_mut(comp_idx) {
            comp.pid = Some(child.id());
            comp.status = ComponentStatus::Running;
        }
    }

    // stdout reader
    if let Some(stdout) = child.stdout.take() {
        let buffer = buffer.clone();
        let running = Arc::clone(&running);
        let components = Arc::clone(&components);
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                if !running.load(Ordering::Relaxed) {
                    break;
                }
                // Detect cargo-watch rebuild
                if line.contains("Compiling") || line.contains("warning[") {
                    let mut comps = components.lock().unwrap();
                    if let Some(comp) = comps.get_mut(comp_idx) {
                        comp.status = ComponentStatus::Rebuilding;
                    }
                } else if line.contains("Running `")
                    || line.contains("Listening")
                    || line.contains("started")
                {
                    let mut comps = components.lock().unwrap();
                    if let Some(comp) = comps.get_mut(comp_idx) {
                        comp.status = ComponentStatus::Running;
                    }
                }
                buffer.push(line, false);
            }
        });
    }

    // stderr reader
    if let Some(stderr) = child.stderr.take() {
        let buffer = buffer.clone();
        let running = Arc::clone(&running);
        let components = Arc::clone(&components);
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                if !running.load(Ordering::Relaxed) {
                    break;
                }
                if line.contains("Compiling") || line.contains("warning[") {
                    let mut comps = components.lock().unwrap();
                    if let Some(comp) = comps.get_mut(comp_idx) {
                        comp.status = ComponentStatus::Rebuilding;
                    }
                } else if line.contains("Finished") {
                    let mut comps = components.lock().unwrap();
                    if let Some(comp) = comps.get_mut(comp_idx) {
                        comp.status = ComponentStatus::Running;
                    }
                }
                buffer.push(line, true);
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

    // Build ComponentInfo list for TUI
    let comp_infos: Vec<ComponentInfo> = configs
        .iter()
        .map(|c| ComponentInfo {
            label: c.label.to_string(),
            url: c.url.to_string(),
            pid: None,
            color_idx: c.color_idx,
            buffer: LogBuffer::new(),
            status: ComponentStatus::Starting,
        })
        .collect();

    let shared_comps = Arc::new(Mutex::new(comp_infos));

    // Spawn all components
    let mut children: Vec<std::process::Child> = Vec::new();
    for (i, config) in configs.iter().enumerate() {
        let buffer = {
            let comps = shared_comps.lock().unwrap();
            comps[i].buffer.clone()
        };
        let child = spawn_component(
            config,
            Arc::clone(&running),
            buffer,
            Arc::clone(&shared_comps),
            i,
        )?;
        children.push(child);
    }

    // Run TUI (blocks until q/Ctrl+C)
    let mut app = App::new(Arc::clone(&shared_comps));
    let _quit_type = super::tui::run_tui(&mut app);

    // Shutdown
    running.store(false, Ordering::Relaxed);
    for child in children.iter_mut() {
        let _ = child.kill();
        let _ = child.wait();
    }

    Ok(())
}
