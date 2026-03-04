//! `pm dev` — run k3rs components in dev mode with colored multiplexed output.
//!
//! - Server/Agent/Vpc: `cargo watch -x "run --bin <bin> -- <args>"`
//! - UI: `dx serve --package k3rs-ui`
//!
//! Press Ctrl+C to stop everything.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};

use super::types::ComponentName;

// ── ANSI colors ─────────────────────────────────────────────

const COLORS: &[&str] = &[
    "\x1b[36m", // cyan    (server)
    "\x1b[33m", // yellow  (agent)
    "\x1b[35m", // magenta (vpc)
    "\x1b[32m", // green   (ui)
];
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";

// ── Dev config ──────────────────────────────────────────────

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
        args: ["--port","6443","--token","demo-token-123","--data-dir","/tmp/k3rs-data","--node-name","master-1"]
            .iter().map(|s| s.to_string()).collect(),
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
        args: ["--server","http://127.0.0.1:6443","--token","demo-token-123","--node-name","node-1",
               "--proxy-port","6444","--service-proxy-port","10256","--dns-port","5353"]
            .iter().map(|s| s.to_string()).collect(),
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
        args: ["--server-url","http://127.0.0.1:6443","--token","demo-token-123"]
            .iter().map(|s| s.to_string()).collect(),
        watch_dirs: vec!["pkg/", "cmd/k3rs-vpc"],
        env: [("RUST_LOG".into(), "debug".into())].into(),
        url: "unix:///run/k3rs-vpc.sock",
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

// ── Kill existing port holders ──────────────────────────────

fn kill_ports(configs: &[DevConfig]) {
    let mut killed: Vec<(u16, u32)> = Vec::new();

    for config in configs {
        for &port in &config.ports {
            // Only kill processes that are LISTENING on the port, not just connected
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
                        killed.push((port, pid));
                    }
                }
            }
        }
    }

    if !killed.is_empty() {
        eprintln!(
            "{}Killed {} process(es) on ports: {}{}",
            DIM,
            killed.len(),
            killed.iter().map(|(p, pid)| format!(":{} (pid {})", p, pid)).collect::<Vec<_>>().join(", "),
            RESET,
        );
        std::thread::sleep(Duration::from_millis(300));
    }
}

// ── Process spawning ────────────────────────────────────────

fn spawn_component(
    config: &DevConfig,
    running: Arc<AtomicBool>,
) -> Result<std::process::Child> {
    let is_ui = config.bin_name.is_empty();

    let mut child = if is_ui {
        Command::new("dx")
            .args(["serve", "--package", "k3rs-ui"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?
    } else {
        let mut run_cmd = format!("run --bin {} --", config.bin_name);
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

    let color = COLORS[config.color_idx % COLORS.len()];
    let label = config.label;
    let prefix = format!("{}{}{:<8}{} ", color, BOLD, label, RESET);

    // stdout reader
    if let Some(stdout) = child.stdout.take() {
        let prefix = prefix.clone();
        let running = Arc::clone(&running);
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                if !running.load(Ordering::Relaxed) {
                    break;
                }
                println!("{}{}", prefix, line);
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
                println!("{}{}", prefix, line);
            }
        });
    }

    Ok(child)
}

// ── Tool check ──────────────────────────────────────────────

fn check_tools(components: &[ComponentName]) -> Result<()> {
    let needs_cargo_watch = components.iter().any(|c| {
        matches!(c, ComponentName::Server | ComponentName::Agent | ComponentName::Vpc)
    });
    let needs_dx = components.iter().any(|c| matches!(c, ComponentName::Ui));

    if needs_cargo_watch {
        let s = Command::new("cargo").arg("watch").arg("--version")
            .stdout(Stdio::null()).stderr(Stdio::null()).status();
        if s.is_err() || !s.unwrap().success() {
            bail!("cargo-watch not installed. Run: cargo install cargo-watch");
        }
    }
    if needs_dx {
        let s = Command::new("dx").arg("--version")
            .stdout(Stdio::null()).stderr(Stdio::null()).status();
        if s.is_err() || !s.unwrap().success() {
            bail!("dioxus-cli (dx) not installed. Run: cargo install dioxus-cli");
        }
    }
    Ok(())
}

// ── Main entry ──────────────────────────────────────────────

pub fn run(component: &ComponentName) -> Result<()> {
    let components = component.resolve();
    check_tools(&components)?;

    let configs: Vec<DevConfig> = components.iter().map(|c| match c {
        ComponentName::Server => server_config(),
        ComponentName::Agent => agent_config(),
        ComponentName::Vpc => vpc_config(),
        ComponentName::Ui => ui_config(),
        ComponentName::All => unreachable!(),
    }).collect();

    // Kill any processes already using our ports
    kill_ports(&configs);

    // Print banner
    println!("{}{}k3rs dev{} — starting {} component(s)", BOLD, COLORS[0], RESET, configs.len());
    for config in &configs {
        let color = COLORS[config.color_idx % COLORS.len()];
        println!("  {}{}{:<8}{}  {}", color, BOLD, config.label, RESET, config.url);
    }
    println!("{}Press Ctrl+C to stop all.{}\n", DIM, RESET);

    let running = Arc::new(AtomicBool::new(true));

    // Ctrl+C handler
    let r = Arc::clone(&running);
    ctrlc::set_handler(move || {
        r.store(false, Ordering::Relaxed);
    })?;

    // Spawn all components
    let mut children: Vec<std::process::Child> = Vec::new();
    for config in &configs {
        let child = spawn_component(config, Arc::clone(&running))?;
        let color = COLORS[config.color_idx % COLORS.len()];
        println!("{}{}Started {}{} (pid {})", color, BOLD, config.label, RESET, child.id());
        children.push(child);
    }
    println!();

    // Wait until Ctrl+C
    while running.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(200));

        // Check for crashed children
        for (i, child) in children.iter_mut().enumerate() {
            if let Ok(Some(exit)) = child.try_wait() {
                if !exit.success() && i < configs.len() {
                    let color = COLORS[configs[i].color_idx % COLORS.len()];
                    eprintln!("\n{}{}{} exited with {}{}", color, BOLD, configs[i].label, exit, RESET);
                }
            }
        }
    }

    // Shutdown
    println!("\n{}Stopping all components...{}", DIM, RESET);
    for (i, child) in children.iter_mut().enumerate() {
        let _ = child.kill();
        let _ = child.wait();
        if i < configs.len() {
            let color = COLORS[configs[i].color_idx % COLORS.len()];
            println!("  {}{}Stopped {}{}", color, BOLD, configs[i].label, RESET);
        }
    }

    Ok(())
}
