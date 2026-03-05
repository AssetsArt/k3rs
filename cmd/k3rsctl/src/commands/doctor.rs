use pkg_types::node::Node;
use pkg_types::pod::{Pod, PodStatus};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

fn pass(msg: &str) {
    println!("  {GREEN}[PASS]{RESET} {msg}");
}
fn warn(msg: &str) {
    println!("  {YELLOW}[WARN]{RESET} {msg}");
}
fn fail(msg: &str) {
    println!("  {RED}[FAIL]{RESET} {msg}");
}

pub async fn handle(client: &reqwest::Client, base: &str) -> anyhow::Result<()> {
    let mut passes = 0u32;
    let mut warns = 0u32;
    let mut fails = 0u32;

    // ── Server Connectivity ─────────────────────────────────────────
    println!("{BOLD}Server Connectivity{RESET}");

    let server_ok = match client
        .get(format!("{base}/api/v1/cluster/info"))
        .timeout(Duration::from_secs(5))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            pass(&format!("API server reachable at {base}"));
            passes += 1;

            if let Ok(info) = resp.json::<serde_json::Value>().await {
                if let Some(v) = info["version"].as_str() {
                    pass(&format!("Cluster version: {v}"));
                    passes += 1;
                }
                if let Some(store) = info["state_store"].as_str() {
                    pass(&format!("State store: {store}"));
                    passes += 1;
                }
            }
            true
        }
        Ok(resp) => {
            fail(&format!("API server returned HTTP {}", resp.status()));
            fails += 1;
            false
        }
        Err(e) => {
            fail(&format!("Cannot reach API server at {base}: {e}"));
            fails += 1;
            false
        }
    };
    println!();

    // ── Nodes ───────────────────────────────────────────────────────
    println!("{BOLD}Nodes{RESET}");

    if server_ok {
        match client
            .get(format!("{base}/api/v1/nodes"))
            .timeout(Duration::from_secs(5))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                let nodes: Vec<Node> = resp.json().await.unwrap_or_default();
                if nodes.is_empty() {
                    warn("No nodes registered");
                    warns += 1;
                } else {
                    let total = nodes.len();
                    let ready = nodes
                        .iter()
                        .filter(|n| n.status == pkg_types::node::NodeStatus::Ready)
                        .count();
                    let unknown = nodes
                        .iter()
                        .filter(|n| n.status == pkg_types::node::NodeStatus::Unknown)
                        .count();
                    let not_ready = nodes
                        .iter()
                        .filter(|n| n.status == pkg_types::node::NodeStatus::NotReady)
                        .count();
                    let unschedulable = nodes.iter().filter(|n| n.unschedulable).count();

                    if ready == total {
                        pass(&format!("All {total} node(s) Ready"));
                        passes += 1;
                    } else {
                        warn(&format!("{ready}/{total} node(s) Ready"));
                        warns += 1;
                    }

                    if unknown > 0 {
                        fail(&format!("{unknown} node(s) in Unknown state"));
                        fails += 1;
                        for n in nodes.iter().filter(|n| n.status == pkg_types::node::NodeStatus::Unknown) {
                            println!("         - {}", n.name);
                        }
                    }
                    if not_ready > 0 {
                        warn(&format!("{not_ready} node(s) NotReady"));
                        warns += 1;
                        for n in nodes.iter().filter(|n| n.status == pkg_types::node::NodeStatus::NotReady) {
                            println!("         - {}", n.name);
                        }
                    }
                    if unschedulable > 0 {
                        warn(&format!("{unschedulable} node(s) cordoned (unschedulable)"));
                        warns += 1;
                    }
                }
            }
            _ => {
                fail("Could not fetch node list");
                fails += 1;
            }
        }
    } else {
        warn("Skipped (server unreachable)");
        warns += 1;
    }
    println!();

    // ── Pods ────────────────────────────────────────────────────────
    println!("{BOLD}Pods{RESET}");

    if server_ok {
        match client
            .get(format!("{base}/api/v1/pods"))
            .timeout(Duration::from_secs(5))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                let pods: Vec<Pod> = resp.json().await.unwrap_or_default();
                let total = pods.len();
                let running = pods
                    .iter()
                    .filter(|p| p.status == PodStatus::Running)
                    .count();
                let pending = pods
                    .iter()
                    .filter(|p| p.status == PodStatus::Pending)
                    .count();
                let failed = pods
                    .iter()
                    .filter(|p| p.status == PodStatus::Failed)
                    .count();

                if total == 0 {
                    pass("No pods deployed");
                    passes += 1;
                } else {
                    pass(&format!("{total} pod(s) total, {running} running"));
                    passes += 1;
                }

                if pending > 0 {
                    warn(&format!("{pending} pod(s) Pending"));
                    warns += 1;
                    for p in pods.iter().filter(|p| p.status == PodStatus::Pending) {
                        println!(
                            "         - {}/{} ({})",
                            p.namespace, p.name,
                            p.node_name.as_deref().unwrap_or("unscheduled")
                        );
                    }
                }
                if failed > 0 {
                    fail(&format!("{failed} pod(s) Failed"));
                    fails += 1;
                    for p in pods.iter().filter(|p| p.status == PodStatus::Failed) {
                        println!("         - {}/{}", p.namespace, p.name);
                    }
                }
            }
            _ => {
                fail("Could not fetch pod list");
                fails += 1;
            }
        }
    } else {
        warn("Skipped (server unreachable)");
        warns += 1;
    }
    println!();

    // ── Local Environment ───────────────────────────────────────────
    println!("{BOLD}Local Environment{RESET}");

    // Data directory
    let data_dir = pkg_constants::paths::DATA_DIR;
    if Path::new(data_dir).is_dir() {
        pass(&format!("Data directory exists: {data_dir}"));
        passes += 1;
    } else {
        warn(&format!("Data directory missing: {data_dir}"));
        warns += 1;
    }

    // Config directory
    let config_dir = pkg_constants::paths::CONFIG_DIR;
    if Path::new(config_dir).is_dir() {
        pass(&format!("Config directory exists: {config_dir}"));
        passes += 1;
    } else {
        warn(&format!("Config directory missing: {config_dir}"));
        warns += 1;
    }

    // VPC socket
    let vpc_sock = pkg_constants::paths::VPC_SOCKET;
    if Path::new(vpc_sock).exists() {
        pass(&format!("VPC socket exists: {vpc_sock}"));
        passes += 1;
    } else {
        warn(&format!("VPC socket not found: {vpc_sock}"));
        warns += 1;
    }

    // DNS VIP reachability (UDP probe)
    let dns_vip = pkg_constants::network::DNS_VIP;
    let dns_addr = format!("[{dns_vip}]:53");
    match tokio::net::UdpSocket::bind("[::]:0").await {
        Ok(sock) => match sock.send_to(&[], dns_addr.parse::<std::net::SocketAddr>().unwrap_or_else(|_| {
            // fallback — shouldn't happen
            "[::1]:53".parse().unwrap()
        })).await {
            Ok(_) => {
                pass(&format!("DNS VIP bindable: {dns_vip}"));
                passes += 1;
            }
            Err(_) => {
                warn(&format!("DNS VIP unreachable: {dns_vip}"));
                warns += 1;
            }
        },
        Err(_) => {
            warn("Could not create UDP socket for DNS check");
            warns += 1;
        }
    }

    // Container runtime
    let runtime = pkg_constants::runtime::DEFAULT_RUNTIME;
    let runtime_in_path = which(runtime);
    if runtime_in_path {
        pass(&format!("Container runtime '{runtime}' found in PATH"));
        passes += 1;
    } else {
        // Check data dir
        let runtime_bin = format!("{}/runtime/{}", data_dir, runtime);
        if Path::new(&runtime_bin).exists() {
            pass(&format!("Container runtime found: {runtime_bin}"));
            passes += 1;
        } else {
            warn(&format!("Container runtime '{runtime}' not found"));
            warns += 1;
        }
    }
    println!();

    // ── Privileges & Capabilities ─────────────────────────────────
    println!("{BOLD}Privileges & Capabilities{RESET}");

    let is_root = Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
        .unwrap_or(false);
    if is_root {
        pass("Running as root — all capabilities available");
        passes += 1;
    } else {
        warn("Not running as root — checking alternatives");
        warns += 1;
    }

    // Check required CLI tools
    for tool in &["ip", "nsenter", "wg", "setcap", "getcap"] {
        if which(tool) {
            pass(&format!("'{tool}' found in PATH"));
            passes += 1;
        } else {
            let severity = match *tool {
                "setcap" | "getcap" => {
                    warn(&format!("'{tool}' not found (install libcap2-bin / libcap)"));
                    warns += 1;
                    "warn"
                }
                "wg" => {
                    warn(&format!("'{tool}' not found (install wireguard-tools) — cross-node traffic disabled"));
                    warns += 1;
                    "warn"
                }
                _ => {
                    fail(&format!("'{tool}' not found — required for pod networking"));
                    fails += 1;
                    "fail"
                }
            };
            let _ = severity;
        }
    }

    // Check file capabilities on installed binaries (non-root only)
    if !is_root {
        let bins_dir = crate::pm::registry::bins_dir();
        let components: &[(&str, &str)] = &[
            ("agent", "k3rs-agent"),
            ("vpc", "k3rs-vpc"),
        ];

        for (key, bin_name) in components {
            let bin_path = bins_dir.join(bin_name);
            if !bin_path.exists() {
                // not installed via PM — check PATH
                if let Some(path) = find_in_path(bin_name) {
                    check_file_caps(&path, key, &mut passes, &mut warns, &mut fails);
                }
                // skip if not found at all — PM section will catch it
            } else {
                check_file_caps(&bin_path, key, &mut passes, &mut warns, &mut fails);
            }
        }

        // Check if systemd is available as an alternative
        if which("systemctl") {
            pass("systemd available — can use AmbientCapabilities via 'k3rsctl pm startup'");
            passes += 1;
        } else {
            warn("systemd not found — binaries need file capabilities (setcap) or run as root");
            warns += 1;
        }
    }
    println!();

    // ── PM Components ───────────────────────────────────────────────
    println!("{BOLD}Process Manager{RESET}");

    match crate::pm::registry::load() {
        Ok(reg) => {
            if reg.processes.is_empty() {
                warn("No components registered in PM");
                warns += 1;
            } else {
                for (key, entry) in &reg.processes {
                    let alive = entry
                        .pid
                        .is_some_and(|pid| crate::pm::lifecycle::is_alive(pid));
                    match entry.status {
                        crate::pm::types::ProcessStatus::Running if alive => {
                            pass(&format!("{key} running (PID {})", entry.pid.unwrap()));
                            passes += 1;
                        }
                        crate::pm::types::ProcessStatus::Running => {
                            fail(&format!("{key} registered as running but process is dead"));
                            fails += 1;
                        }
                        crate::pm::types::ProcessStatus::Stopped => {
                            warn(&format!("{key} stopped"));
                            warns += 1;
                        }
                        crate::pm::types::ProcessStatus::Crashed => {
                            fail(&format!(
                                "{key} crashed (restarts: {})",
                                entry.restart_count
                            ));
                            fails += 1;
                        }
                        _ => {
                            warn(&format!("{key}: {:?}", entry.status));
                            warns += 1;
                        }
                    }
                }
            }
        }
        Err(_) => {
            warn("PM registry not initialized (run `k3rsctl pm install` first)");
            warns += 1;
        }
    }
    println!();

    // ── Summary ─────────────────────────────────────────────────────
    println!("{BOLD}Summary{RESET}");
    println!(
        "  {GREEN}{passes} passed{RESET}, {YELLOW}{warns} warning(s){RESET}, {RED}{fails} failed{RESET}"
    );

    if fails > 0 || warns > 0 {
        println!();
        println!("  {BOLD}Quick fixes:{RESET}");
        println!();
        println!("  {BOLD}Development (run as root):{RESET}");
        println!("    sudo cargo run -p k3rs-agent");
        println!();
        println!("  {BOLD}Development (without root):{RESET}");
        println!("    cargo build --release -p k3rs-agent -p k3rs-vpc");
        println!("    sudo setcap 'cap_net_admin,cap_net_raw,cap_sys_admin,cap_sys_ptrace,cap_dac_override+eip' target/release/k3rs-agent");
        println!("    sudo setcap 'cap_net_admin,cap_bpf,cap_sys_admin,cap_perfmon+eip' target/release/k3rs-vpc");
        println!();
        println!("  {BOLD}Production (systemd):{RESET}");
        println!("    k3rsctl pm install agent");
        println!("    k3rsctl pm startup          # generates systemd units with AmbientCapabilities");
        println!("    sudo systemctl start k3rs-agent");
        println!();
        println!("  {BOLD}Missing tools:{RESET}");
        println!("    sudo apt install iproute2 libcap2-bin wireguard-tools  # Debian/Ubuntu");
        println!("    sudo dnf install iproute libcap wireguard-tools        # Fedora");
    }

    if fails > 0 {
        println!();
        println!("  Run with {BOLD}--server <url>{RESET} if the server is on a different host.");
        std::process::exit(1);
    } else if warns > 0 {
        println!();
        println!("  Some warnings found — cluster may still be functional.");
    } else {
        println!();
        println!("  {GREEN}Everything looks good!{RESET}");
    }

    Ok(())
}

fn which(name: &str) -> bool {
    find_in_path(name).is_some()
}

fn find_in_path(name: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(name))
            .find(|p| p.is_file())
    })
}

/// Check file capabilities on a binary using `getcap` and compare with required caps.
fn check_file_caps(
    bin: &Path,
    component_key: &str,
    passes: &mut u32,
    warns: &mut u32,
    fails: &mut u32,
) {
    let required = pkg_constants::capabilities::caps_for_component(component_key);
    if required.is_empty() {
        return;
    }

    let bin_display = bin.display();

    let output = Command::new("getcap").arg(bin).output();
    match output {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            let stdout_lower = stdout.to_lowercase();

            let mut missing: Vec<&str> = Vec::new();
            for cap in required {
                if !stdout_lower.contains(&cap.to_lowercase()) {
                    missing.push(cap);
                }
            }

            if missing.is_empty() {
                pass(&format!("{bin_display}: all capabilities set"));
                *passes += 1;
            } else {
                let missing_str = missing.join(", ");
                fail(&format!("{bin_display}: missing capabilities: {missing_str}"));
                *fails += 1;
                let cap_str = required
                    .iter()
                    .map(|c| c.to_lowercase())
                    .collect::<Vec<_>>()
                    .join(",");
                println!(
                    "         fix: sudo setcap '{cap_str}+eip' {bin_display}"
                );
            }
        }
        Ok(_) => {
            warn(&format!("{bin_display}: getcap failed (binary may need setcap)"));
            *warns += 1;
            let cap_str = required
                .iter()
                .map(|c| c.to_lowercase())
                .collect::<Vec<_>>()
                .join(",");
            println!(
                "         fix: sudo setcap '{cap_str}+eip' {bin_display}"
            );
        }
        Err(_) => {
            warn(&format!("{bin_display}: getcap not available, cannot verify capabilities"));
            *warns += 1;
        }
    }
}
