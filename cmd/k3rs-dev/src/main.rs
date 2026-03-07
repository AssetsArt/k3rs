//! `k3rs-dev` — run k3rs components in dev mode with auto-rebuild on code changes.
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
use clap::Parser;

#[derive(Clone, Debug, PartialEq, Eq, clap::ValueEnum)]
enum Component {
    Server,
    Agent,
    Vpc,
    Ui,
    All,
}

impl Component {
    const ALL_COMPONENTS: &[Component] = &[
        Component::Server,
        Component::Agent,
        Component::Vpc,
        Component::Ui,
    ];

    fn resolve(&self) -> Vec<Component> {
        match self {
            Self::All => Self::ALL_COMPONENTS.to_vec(),
            other => vec![other.clone()],
        }
    }
}

#[derive(Parser)]
#[command(
    name = "k3rs-dev",
    about = "Run k3rs components in dev mode with auto-rebuild"
)]
struct Cli {
    /// Component(s) to run in dev mode (server, agent, vpc, ui, or all)
    component: Component,
}

// ── Dev config ──────────────────────────────────────────────────

struct DevConfig {
    label: &'static str,
    bin_name: &'static str,
    args: Vec<String>,
    watch_dirs: Vec<&'static str>,
    env: HashMap<String, String>,
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
        ports: vec![
            pkg_constants::network::DEFAULT_TUNNEL_PORT,
            pkg_constants::network::DEFAULT_SERVICE_PROXY_PORT,
            pkg_constants::network::DEFAULT_DNS_PORT,
        ],
    }
}

fn vpc_config() -> DevConfig {
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

fn check_tools(components: &[Component]) -> Result<()> {
    let needs_cargo_watch = components
        .iter()
        .any(|c| matches!(c, Component::Server | Component::Agent | Component::Vpc));
    let needs_dx = components.iter().any(|c| matches!(c, Component::Ui));

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

// ── Preflight checks ─────────────────────────────────────────────

/// Verify we're running inside the k3rs workspace root (has Cargo.toml with [workspace]).
fn check_workspace_root() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let manifest = cwd.join("Cargo.toml");

    if !manifest.exists() {
        bail!(
            "Not in a Rust project directory.\n\n\
             To get started:\n\
             \n\
             git clone https://github.com/{} k3rs\n\
             cd k3rs\n\
             k3rs-dev all\n",
            pkg_constants::network::GITHUB_REPO
        );
    }

    let content = std::fs::read_to_string(&manifest)?;
    if !content.contains("[workspace]") {
        bail!(
            "Not in the k3rs workspace root (found Cargo.toml but no [workspace] section).\n\n\
             cd into the root of the k3rs repo:\n\
             \n\
             cd /path/to/k3rs\n\
             k3rs-dev all\n"
        );
    }

    // Sanity check: make sure it's actually the k3rs workspace (has cmd/k3rs-server)
    if !cwd.join("cmd/k3rs-server").is_dir() {
        bail!(
            "This workspace does not look like the k3rs project.\n\n\
             Clone the official repo:\n\
             \n\
             git clone https://github.com/{} k3rs\n\
             cd k3rs\n\
             k3rs-dev all\n",
            pkg_constants::network::GITHUB_REPO
        );
    }

    Ok(())
}

/// macOS: codesign k3rs-vmm with Virtualization.framework entitlements.
/// The agent spawns k3rs-vmm as a child process, so it must be signed.
#[cfg(target_os = "macos")]
fn ensure_entitlements(components: &[Component]) -> Result<()> {
    if !components.iter().any(|c| matches!(c, Component::Agent)) {
        return Ok(());
    }

    let vmm_bin = "target/debug/k3rs-vmm";
    let entitlements = "cmd/k3rs-vmm/k3rs-vmm.entitlements";

    // Build k3rs-vmm if needed
    if !std::path::Path::new(vmm_bin).exists() {
        println!("Building k3rs-vmm for codesigning...");
        let status = Command::new("cargo")
            .args(["build", "-p", "k3rs-vmm"])
            .status()?;
        if !status.success() {
            bail!("cargo build -p k3rs-vmm failed");
        }
    }

    // Check if already signed with correct entitlements
    let already_signed = Command::new("codesign")
        .args(["-d", "--entitlements", "-", vmm_bin])
        .output()
        .map(|o| {
            o.status.success()
                && String::from_utf8_lossy(&o.stdout).contains("com.apple.security.virtualization")
        })
        .unwrap_or(false);

    if already_signed {
        return Ok(());
    }

    println!("Signing k3rs-vmm with Virtualization.framework entitlements...");
    let status = Command::new("codesign")
        .args([
            "--entitlements",
            entitlements,
            "--force",
            "-s",
            "-",
            vmm_bin,
        ])
        .status()?;

    if !status.success() {
        bail!(
            "codesign failed for {}.\n\
             Ensure Xcode command-line tools are installed: xcode-select --install",
            vmm_bin
        );
    }

    println!("  {} signed with {}", vmm_bin, entitlements);
    Ok(())
}

/// Ensure Linux capabilities are set on binaries that need privileged operations.
/// Automatically builds and runs `sudo setcap` so the user just runs `k3rs-dev all`.
#[cfg(target_os = "linux")]
fn ensure_capabilities(components: &[Component]) -> Result<()> {
    let privileged: Vec<(&str, &str)> = components
        .iter()
        .filter_map(|c| match c {
            Component::Agent => Some(("agent", "k3rs-agent")),
            Component::Vpc => Some(("vpc", "k3rs-vpc")),
            _ => None,
        })
        .collect();

    if privileged.is_empty() {
        return Ok(());
    }

    // Running as root — no caps needed
    let is_root = Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
        .unwrap_or(false);

    if is_root {
        return Ok(());
    }

    // Check which binaries need caps
    let mut needs_setcap: Vec<(&str, &str)> = Vec::new();

    for &(key, bin_name) in &privileged {
        let required = pkg_constants::capabilities::caps_for_component(key);
        if required.is_empty() {
            continue;
        }

        let debug_bin = format!("target/debug/{}", bin_name);
        if std::path::Path::new(&debug_bin).exists() && check_file_has_caps(&debug_bin, required) {
            continue;
        }

        needs_setcap.push((key, bin_name));
    }

    if needs_setcap.is_empty() {
        return Ok(());
    }

    // Build the binaries first so setcap has something to apply to
    let packages: Vec<&str> = needs_setcap.iter().map(|(_, bin)| *bin).collect();
    println!("Building privileged components: {}", packages.join(", "));

    let mut build_cmd = Command::new("cargo");
    build_cmd.arg("build");
    for pkg in &packages {
        build_cmd.args(["-p", pkg]);
    }
    let status = build_cmd.status()?;
    if !status.success() {
        bail!("cargo build failed");
    }

    // Apply capabilities via sudo setcap
    println!("Granting network capabilities (sudo required)...");

    for &(key, bin_name) in &needs_setcap {
        let caps = pkg_constants::capabilities::caps_for_component(key);
        let cap_str = caps
            .iter()
            .map(|c| c.to_lowercase())
            .collect::<Vec<_>>()
            .join(",")
            + "+eip";

        let debug_bin = format!("target/debug/{}", bin_name);

        let status = Command::new("sudo")
            .args(["setcap", &cap_str, &debug_bin])
            .status()?;

        if !status.success() {
            bail!(
                "Failed to set capabilities on {}.\n\
                 You can also run as root: sudo k3rs-dev all",
                debug_bin
            );
        }

        println!("  {} -> {}", bin_name, cap_str);
    }

    Ok(())
}

/// Check if a binary has the required capabilities set via getcap.
#[cfg(target_os = "linux")]
fn check_file_has_caps(path: &str, required: &[&str]) -> bool {
    let output = Command::new("getcap").arg(path).output();
    match output {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout).to_lowercase();
            required
                .iter()
                .all(|cap| stdout.contains(&cap.to_lowercase()))
        }
        _ => false,
    }
}

// ── Main ────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let cli = Cli::parse();
    let components = cli.component.resolve();

    check_workspace_root()?;
    check_tools(&components)?;

    #[cfg(target_os = "linux")]
    ensure_capabilities(&components)?;

    #[cfg(target_os = "macos")]
    ensure_entitlements(&components)?;

    let configs: Vec<DevConfig> = components
        .iter()
        .map(|c| match c {
            Component::Server => server_config(),
            Component::Agent => agent_config(),
            Component::Vpc => vpc_config(),
            Component::Ui => ui_config(),
            Component::All => unreachable!(),
        })
        .collect();

    kill_ports(&configs);

    let running = Arc::new(AtomicBool::new(true));

    let r = Arc::clone(&running);
    ctrlc::set_handler(move || {
        println!("\nReceived Ctrl-C, shutting down...");
        r.store(false, Ordering::Relaxed);
    })
    .expect("Error setting Ctrl-C handler");

    let mut children: Vec<std::process::Child> = Vec::new();
    for config in configs.iter() {
        let child = spawn_component(config, Arc::clone(&running))?;
        children.push(child);
    }

    println!("All components started. Logs will appear below (Ctrl+C to quit)...");

    while running.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    println!("Stopping processes...");
    for child in children.iter_mut() {
        let _ = child.kill();
        let _ = child.wait();
    }

    Ok(())
}
