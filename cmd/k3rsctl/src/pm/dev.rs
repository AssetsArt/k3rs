//! `pm dev` — run k3rs components in dev mode with a split-screen TUI.
//!
//! Left pane:  live multiplexed logs from all running components.
//! Right pane: cluster state dashboard (URLs, components, pods, nodes, VPCs).
//!
//! - Server/Agent/Vpc: `cargo watch -x "run --bin <bin> -- <args>"`
//! - UI: `dx serve --package k3rs-ui`
//!
//! Press `q` or Ctrl+C to stop everything.

use std::collections::{HashMap, VecDeque};
use std::io::{self, BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::prelude::*;
use ratatui::widgets::*;

use super::types::ComponentName;

const MAX_LOG_LINES: usize = 2000;
const STATE_POLL_INTERVAL: Duration = Duration::from_secs(3);
const API_BASE: &str = "http://127.0.0.1:6443";
const API_TOKEN: &str = "demo-token-123";

// ── Log line ──────────────────────────────────────────────────

#[derive(Clone)]
struct LogLine {
    label: String,
    color: Color,
    text: String,
}

// ── Component process state ──────────────────────────────────

#[derive(Clone)]
struct CompState {
    label: String,
    color: Color,
    status: String,
    pid: Option<u32>,
    url: String,
}

// ── Cluster state (fetched from API) ─────────────────────────

#[derive(Default, Clone)]
#[allow(dead_code)]
struct ClusterState {
    nodes: Vec<(String, String)>,   // (name, status)
    pods: Vec<(String, String)>,    // (name, status)
    vpcs: Vec<(String, String)>,    // (name, cidr)
    services: Vec<(String, String)>,// (name, type)
    last_fetch: Option<Instant>,
    error: Option<String>,
}

// ── Dev config ───────────────────────────────────────────────

struct DevConfig {
    label: &'static str,
    bin_name: &'static str,
    args: Vec<String>,
    watch_dirs: Vec<&'static str>,
    env: HashMap<String, String>,
    url: &'static str,
    color: Color,
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
        color: Color::Cyan,
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
        color: Color::Yellow,
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
        color: Color::Magenta,
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
        color: Color::Green,
        ports: vec![8080],
    }
}

// ── Shared state ─────────────────────────────────────────────

struct SharedState {
    logs: VecDeque<LogLine>,
    components: Vec<CompState>,
    cluster: ClusterState,
    log_scroll: u16,
}

impl SharedState {
    fn push_log(&mut self, label: &str, color: Color, text: String) {
        self.logs.push_back(LogLine {
            label: label.to_string(),
            color,
            text,
        });
        while self.logs.len() > MAX_LOG_LINES {
            self.logs.pop_front();
        }
    }
}

// ── Kill existing port holders ───────────────────────────────

fn kill_ports(configs: &[DevConfig]) {
    let mut killed: Vec<(u16, u32)> = Vec::new();

    for config in configs {
        for &port in &config.ports {
            // Use lsof to find PIDs listening on this port
            if let Ok(output) = Command::new("lsof")
                .args(["-ti", &format!(":{}", port)])
                .output()
            {
                let stdout = String::from_utf8_lossy(&output.stdout);
                for pid_str in stdout.split_whitespace() {
                    if let Ok(pid) = pid_str.parse::<u32>() {
                        // Don't kill ourselves
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
            "Killed {} process(es) on ports: {}",
            killed.len(),
            killed.iter().map(|(p, pid)| format!(":{} (pid {})", p, pid)).collect::<Vec<_>>().join(", ")
        );
        // Brief pause for OS to release the ports
        std::thread::sleep(Duration::from_millis(300));
    }
}

// ── Process spawning ─────────────────────────────────────────

fn spawn_cargo_watch(
    config: &DevConfig,
    state: Arc<Mutex<SharedState>>,
) -> Result<std::process::Child> {
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
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = cmd.spawn()?;
    let label = config.label.to_string();
    let color = config.color;

    // stdout reader
    if let Some(stdout) = child.stdout.take() {
        let state = Arc::clone(&state);
        let label = label.clone();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                state.lock().unwrap().push_log(&label, color, line);
            }
        });
    }

    // stderr reader
    if let Some(stderr) = child.stderr.take() {
        let state = Arc::clone(&state);
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                state.lock().unwrap().push_log(&label, color, line);
            }
        });
    }

    Ok(child)
}

fn spawn_dx_serve(state: Arc<Mutex<SharedState>>) -> Result<std::process::Child> {
    let mut cmd = Command::new("dx");
    cmd.arg("serve")
        .arg("--package")
        .arg("k3rs-ui")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn()?;
    let color = Color::Green;

    if let Some(stdout) = child.stdout.take() {
        let state = Arc::clone(&state);
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                state.lock().unwrap().push_log("ui", color, line);
            }
        });
    }

    if let Some(stderr) = child.stderr.take() {
        let state = Arc::clone(&state);
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                state.lock().unwrap().push_log("ui", color, line);
            }
        });
    }

    Ok(child)
}

// ── Cluster state poller ─────────────────────────────────────

fn start_state_poller(state: Arc<Mutex<SharedState>>) {
    std::thread::spawn(move || {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(2))
            .danger_accept_invalid_certs(true)
            .build()
            .unwrap();

        loop {
            std::thread::sleep(STATE_POLL_INTERVAL);

            let mut cluster = ClusterState {
                last_fetch: Some(Instant::now()),
                ..Default::default()
            };

            let auth = format!("Bearer {}", API_TOKEN);

            // Fetch nodes
            match client.get(format!("{}/api/v1/nodes", API_BASE))
                .header("Authorization", &auth)
                .send()
                .and_then(|r| r.json::<Vec<serde_json::Value>>())
            {
                Ok(nodes) => {
                    for n in &nodes {
                        let name = n["name"].as_str().unwrap_or("-").to_string();
                        let status = n["status"].as_str().unwrap_or("-").to_string();
                        cluster.nodes.push((name, status));
                    }
                }
                Err(e) => cluster.error = Some(format!("nodes: {}", e)),
            }

            // Fetch pods (default ns)
            if let Ok(pods) = client.get(format!("{}/api/v1/namespaces/default/pods", API_BASE))
                .header("Authorization", &auth)
                .send()
                .and_then(|r| r.json::<Vec<serde_json::Value>>())
            {
                for p in &pods {
                    let name = p["name"].as_str().unwrap_or("-").to_string();
                    let status = p["status"].as_str().unwrap_or("-").to_string();
                    cluster.pods.push((name, status));
                }
            }

            // Fetch VPCs
            if let Ok(vpcs) = client.get(format!("{}/api/v1/vpcs", API_BASE))
                .header("Authorization", &auth)
                .send()
                .and_then(|r| r.json::<Vec<serde_json::Value>>())
            {
                for v in &vpcs {
                    let name = v["name"].as_str().unwrap_or("-").to_string();
                    let cidr = v["ipv4_cidr"].as_str().unwrap_or("-").to_string();
                    cluster.vpcs.push((name, cidr));
                }
            }

            // Fetch services (default ns)
            if let Ok(svcs) = client.get(format!("{}/api/v1/namespaces/default/services", API_BASE))
                .header("Authorization", &auth)
                .send()
                .and_then(|r| r.json::<Vec<serde_json::Value>>())
            {
                for s in &svcs {
                    let name = s["name"].as_str().unwrap_or("-").to_string();
                    let stype = s["spec"]["service_type"].as_str().unwrap_or("-").to_string();
                    cluster.services.push((name, stype));
                }
            }

            state.lock().unwrap().cluster = cluster;
        }
    });
}

// ── Tool check ───────────────────────────────────────────────

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

// ── TUI rendering ────────────────────────────────────────────

fn draw(frame: &mut Frame, state: &SharedState) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(frame.area());

    draw_logs(frame, chunks[0], state);
    draw_state(frame, chunks[1], state);
}

fn draw_logs(frame: &mut Frame, area: Rect, state: &SharedState) {
    let block = Block::default()
        .title(" Logs ")
        .title_style(Style::default().fg(Color::White).bold())
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let visible_height = inner.height as usize;
    let total = state.logs.len();
    let scroll = state.log_scroll as usize;

    let start = if total > visible_height + scroll {
        total - visible_height - scroll
    } else {
        0
    };
    let end = if total > scroll { total - scroll } else { 0 };

    let lines: Vec<Line> = state.logs.iter()
        .skip(start)
        .take(end.saturating_sub(start))
        .map(|l| {
            Line::from(vec![
                Span::styled(format!("{:<8}", l.label), Style::default().fg(l.color).bold()),
                Span::raw(" "),
                Span::styled(&l.text, Style::default().fg(Color::Gray)),
            ])
        })
        .collect();

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

fn draw_state(frame: &mut Frame, area: Rect, state: &SharedState) {
    let block = Block::default()
        .title(" State ")
        .title_style(Style::default().fg(Color::White).bold())
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();

    // Components section
    lines.push(Line::styled("COMPONENTS", Style::default().fg(Color::White).bold()));
    lines.push(Line::raw(""));
    for comp in &state.components {
        let status_color = match comp.status.as_str() {
            "running" => Color::Green,
            "starting" => Color::Yellow,
            _ => Color::Red,
        };
        let pid_str = comp.pid.map_or("-".to_string(), |p| p.to_string());
        lines.push(Line::from(vec![
            Span::styled(format!("  {:<8}", comp.label), Style::default().fg(comp.color).bold()),
            Span::styled(format!("{:<10}", comp.status), Style::default().fg(status_color)),
            Span::styled(format!("pid:{}", pid_str), Style::default().fg(Color::DarkGray)),
        ]));
        lines.push(Line::from(vec![
            Span::raw("           "),
            Span::styled(&comp.url, Style::default().fg(Color::Blue)),
        ]));
    }

    // Cluster state section
    lines.push(Line::raw(""));
    lines.push(Line::styled("CLUSTER", Style::default().fg(Color::White).bold()));

    if let Some(ref err) = state.cluster.error {
        lines.push(Line::styled(
            format!("  {}", err),
            Style::default().fg(Color::Red),
        ));
    }

    // Nodes
    if !state.cluster.nodes.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            format!("  Nodes ({})", state.cluster.nodes.len()),
            Style::default().fg(Color::Cyan),
        ));
        for (name, status) in &state.cluster.nodes {
            let sc = if status == "Ready" { Color::Green } else { Color::Yellow };
            lines.push(Line::from(vec![
                Span::raw("    "),
                Span::styled(format!("{:<16}", name), Style::default().fg(Color::Gray)),
                Span::styled(status, Style::default().fg(sc)),
            ]));
        }
    }

    // Pods
    if !state.cluster.pods.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            format!("  Pods ({})", state.cluster.pods.len()),
            Style::default().fg(Color::Yellow),
        ));
        for (name, status) in state.cluster.pods.iter().take(10) {
            let sc = match status.as_str() {
                "Running" => Color::Green,
                "Pending" | "Scheduled" => Color::Yellow,
                _ => Color::Red,
            };
            lines.push(Line::from(vec![
                Span::raw("    "),
                Span::styled(format!("{:<20}", name), Style::default().fg(Color::Gray)),
                Span::styled(status, Style::default().fg(sc)),
            ]));
        }
        if state.cluster.pods.len() > 10 {
            lines.push(Line::styled(
                format!("    ... and {} more", state.cluster.pods.len() - 10),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }

    // VPCs
    if !state.cluster.vpcs.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            format!("  VPCs ({})", state.cluster.vpcs.len()),
            Style::default().fg(Color::Magenta),
        ));
        for (name, cidr) in &state.cluster.vpcs {
            lines.push(Line::from(vec![
                Span::raw("    "),
                Span::styled(format!("{:<16}", name), Style::default().fg(Color::Gray)),
                Span::styled(cidr, Style::default().fg(Color::DarkGray)),
            ]));
        }
    }

    // Services
    if !state.cluster.services.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            format!("  Services ({})", state.cluster.services.len()),
            Style::default().fg(Color::Blue),
        ));
        for (name, stype) in state.cluster.services.iter().take(8) {
            lines.push(Line::from(vec![
                Span::raw("    "),
                Span::styled(format!("{:<20}", name), Style::default().fg(Color::Gray)),
                Span::styled(stype, Style::default().fg(Color::DarkGray)),
            ]));
        }
    }

    // Help line
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "  q:quit  j/k:scroll  G:bottom",
        Style::default().fg(Color::DarkGray),
    ));

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

// ── Main entry ───────────────────────────────────────────────

pub fn run(component: &ComponentName) -> Result<()> {
    let components = component.resolve();
    check_tools(&components)?;

    // Build component configs
    let configs: Vec<DevConfig> = components.iter().map(|c| match c {
        ComponentName::Server => server_config(),
        ComponentName::Agent => agent_config(),
        ComponentName::Vpc => vpc_config(),
        ComponentName::Ui => ui_config(),
        ComponentName::All => unreachable!(),
    }).collect();

    // Initialize shared state
    let state = Arc::new(Mutex::new(SharedState {
        logs: VecDeque::new(),
        components: configs.iter().map(|c| CompState {
            label: c.label.to_string(),
            color: c.color,
            status: "starting".to_string(),
            pid: None,
            url: c.url.to_string(),
        }).collect(),
        cluster: ClusterState::default(),
        log_scroll: 0,
    }));

    // Kill any processes already using our ports
    kill_ports(&configs);

    // Spawn child processes
    let mut children: Vec<std::process::Child> = Vec::new();
    for (i, comp) in components.iter().enumerate() {
        let child = match comp {
            ComponentName::Ui => spawn_dx_serve(Arc::clone(&state))?,
            _ => spawn_cargo_watch(&configs[i], Arc::clone(&state))?,
        };
        // Update PID
        let pid = child.id();
        {
            let mut s = state.lock().unwrap();
            s.components[i].pid = Some(pid);
            s.components[i].status = "running".to_string();
        }
        children.push(child);
    }

    // Start cluster state poller
    start_state_poller(Arc::clone(&state));

    // Setup terminal
    terminal::enable_raw_mode()?;
    crossterm::execute!(io::stdout(), EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    // Main TUI loop
    let result = run_tui_loop(&mut terminal, &state, &mut children);

    // Cleanup: restore terminal
    terminal::disable_raw_mode()?;
    crossterm::execute!(io::stdout(), LeaveAlternateScreen)?;

    // Kill all children
    for child in children.iter_mut() {
        let _ = child.kill();
        let _ = child.wait();
    }

    result
}

fn run_tui_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &Arc<Mutex<SharedState>>,
    children: &mut [std::process::Child],
) -> Result<()> {
    loop {
        // Draw
        {
            let s = state.lock().unwrap();
            terminal.draw(|f| draw(f, &s))?;
        }

        // Poll events (100ms timeout for responsive UI)
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') => return Ok(()),
                        KeyCode::Char('c') if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) => {
                            return Ok(());
                        }
                        KeyCode::Char('j') | KeyCode::Down => {
                            let mut s = state.lock().unwrap();
                            s.log_scroll = s.log_scroll.saturating_sub(1);
                        }
                        KeyCode::Char('k') | KeyCode::Up => {
                            let mut s = state.lock().unwrap();
                            s.log_scroll = s.log_scroll.saturating_add(1);
                        }
                        KeyCode::Char('G') | KeyCode::End => {
                            let mut s = state.lock().unwrap();
                            s.log_scroll = 0;
                        }
                        KeyCode::Char('g') | KeyCode::Home => {
                            let mut s = state.lock().unwrap();
                            let total = s.logs.len() as u16;
                            s.log_scroll = total;
                        }
                        _ => {}
                    }
                }
            }
        }

        // Check if any child has exited, update status
        for (i, child) in children.iter_mut().enumerate() {
            if let Ok(Some(exit)) = child.try_wait() {
                let mut s = state.lock().unwrap();
                if i < s.components.len() {
                    s.components[i].status = if exit.success() {
                        "stopped".to_string()
                    } else {
                        "crashed".to_string()
                    };
                    s.components[i].pid = None;
                }
            }
        }
    }
}
