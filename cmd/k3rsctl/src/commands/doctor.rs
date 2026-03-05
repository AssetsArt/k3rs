use pkg_types::node::Node;
use pkg_types::pod::{Pod, PodStatus};
use std::path::Path;
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
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| dir.join(name).is_file())
        })
        .unwrap_or(false)
}
