//! pfctl NAT setup for routing pod traffic to the internet on macOS.
//!
//! Configures:
//! - IP forwarding (`sysctl net.inet.ip.forwarding=1`)
//! - Route for pod CIDR through utun device
//! - pfctl NAT masquerade rule in a dedicated anchor

use std::io::{self, Write};
use std::process::Command;
use tracing::{info, warn};

/// Determine the default route interface (e.g., "en0").
fn default_interface() -> io::Result<String> {
    let output = Command::new("route")
        .args(["-n", "get", "default"])
        .output()?;

    if !output.status.success() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "route -n get default failed",
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let trimmed = line.trim();
        if let Some(iface) = trimmed.strip_prefix("interface:") {
            return Ok(iface.trim().to_string());
        }
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "no default interface found in route output",
    ))
}

/// Set up pfctl NAT rules + IP forwarding + route for pod traffic.
///
/// - `utun_name`: the utun interface name (e.g., "utun5")
/// - `pod_cidr`: the pod IP range to NAT (e.g., "10.42.0.0/16")
pub fn setup_nat(utun_name: &str, pod_cidr: &str) -> io::Result<()> {
    let ext_if = default_interface()?;

    // 1. Enable IP forwarding
    let _ = Command::new("sysctl")
        .args(["-w", "net.inet.ip.forwarding=1"])
        .output();

    // 2. Configure utun point-to-point addresses
    let status = Command::new("ifconfig")
        .args([
            utun_name,
            pkg_constants::network::UTUN_HOST_IP,
            pkg_constants::network::UTUN_PEER_IP,
        ])
        .status()?;
    if !status.success() {
        warn!("[pfnat] ifconfig {} failed", utun_name);
    }

    // Bring the interface up
    let _ = Command::new("ifconfig")
        .args([utun_name, "up"])
        .status();

    // 3. Add route for pod CIDR through the utun device
    let _ = Command::new("route")
        .args(["add", "-net", pod_cidr, "-interface", utun_name])
        .output();

    // 4. Load pfctl NAT rule into dedicated anchor
    let rule = format!("nat on {} from {} to any -> ({})\n", ext_if, pod_cidr, ext_if);

    let mut child = Command::new("pfctl")
        .args(["-a", pkg_constants::network::PFCTL_ANCHOR, "-f", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;

    if let Some(ref mut stdin) = child.stdin {
        stdin.write_all(rule.as_bytes())?;
    }
    // Drop stdin to close it before wait
    drop(child.stdin.take());
    child.wait()?;

    // 5. Enable PF (idempotent)
    let _ = Command::new("pfctl")
        .args(["-e"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    info!(
        "[pfnat] NAT setup complete: {} from {} via {}",
        utun_name, pod_cidr, ext_if
    );
    Ok(())
}

/// Tear down pfctl NAT rules and routes.
pub fn teardown_nat(utun_name: &str, pod_cidr: &str) {
    // Remove route
    let _ = Command::new("route")
        .args(["delete", "-net", pod_cidr])
        .output();

    // Flush anchor rules
    let _ = Command::new("pfctl")
        .args(["-a", pkg_constants::network::PFCTL_ANCHOR, "-F", "all"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();

    info!("[pfnat] NAT teardown complete for {}", utun_name);
}
