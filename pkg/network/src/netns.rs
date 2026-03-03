//! Per-pod veth setup — creates veth pairs, assigns Ghost IPv6 + guest IPv4,
//! and configures routes inside the container network namespace.
//!
//! Uses `ip` commands and `nsenter` to configure networking inside the
//! container's network namespace, consistent with the existing Firecracker
//! TAP setup pattern.

use anyhow::Result;
use tracing::info;

/// Network configuration for a single pod.
pub struct PodNetworkConfig {
    /// Pod identifier (used to derive veth names).
    pub pod_id: String,
    /// Ghost IPv6 address allocated by k3rs-vpc.
    pub ghost_ipv6: String,
    /// Guest IPv4 address for app compatibility.
    pub guest_ipv4: String,
    /// PID of the container's init process (from OCI create --pid-file).
    pub container_pid: u32,
    /// Bridge to attach the host-side veth to.
    pub bridge_name: String,
}

impl PodNetworkConfig {
    /// Derive the host-side veth name from the pod ID.
    fn veth_host(&self) -> String {
        let short = &self.pod_id[..8.min(self.pod_id.len())];
        format!("veth-{}", short)
    }

    /// Derive the temporary peer name (moved into the netns, then renamed to eth0).
    fn veth_peer(&self) -> String {
        let short = &self.pod_id[..8.min(self.pod_id.len())];
        format!("vethtmp-{}", short)
    }
}

/// Run an `ip` command, returning an error on failure (with stderr context).
async fn run_ip(args: &[&str]) -> Result<()> {
    let output = tokio::process::Command::new("ip")
        .args(args)
        .output()
        .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("ip {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(())
}

/// Run a command inside the container's network namespace via nsenter.
async fn nsenter_run(pid: u32, args: &[&str]) -> Result<()> {
    let pid_str = pid.to_string();
    let mut cmd_args = vec!["-t", &pid_str, "-n", "--"];
    cmd_args.extend_from_slice(args);

    let output = tokio::process::Command::new("nsenter")
        .args(&cmd_args)
        .output()
        .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "nsenter -t {} -n -- {} failed: {}",
            pid,
            args.join(" "),
            stderr.trim()
        );
    }
    Ok(())
}

/// Create veth pair, move peer into container netns, attach host side to
/// bridge, and configure IPv6/IPv4 addresses + routes inside the container.
pub async fn setup_pod_network(config: &PodNetworkConfig) -> Result<()> {
    let veth_host = config.veth_host();
    let veth_peer = config.veth_peer();
    let pid = config.container_pid;

    info!(
        "[netns:{}] Setting up pod network: veth={}, ghost_ipv6={}, guest_ipv4={}, pid={}",
        &config.pod_id[..8.min(config.pod_id.len())],
        veth_host,
        config.ghost_ipv6,
        config.guest_ipv4,
        pid
    );

    // 1. Create veth pair
    run_ip(&[
        "link", "add", &veth_host, "type", "veth", "peer", "name", &veth_peer,
    ])
    .await?;

    // 2. Move peer into container netns
    let pid_str = pid.to_string();
    run_ip(&["link", "set", &veth_peer, "netns", &pid_str]).await?;

    // 3. Attach host side to bridge and bring up
    run_ip(&["link", "set", &veth_host, "master", &config.bridge_name]).await?;
    run_ip(&["link", "set", &veth_host, "up"]).await?;

    // 4. Inside netns: rename peer → eth0, bring up lo + eth0
    nsenter_run(
        pid,
        &["ip", "link", "set", &veth_peer, "name", "eth0"],
    )
    .await?;
    nsenter_run(pid, &["ip", "link", "set", "lo", "up"]).await?;
    nsenter_run(pid, &["ip", "link", "set", "eth0", "up"]).await?;

    // 5. Assign Ghost IPv6 (/128) and guest IPv4 (/32) to eth0
    let ipv6_cidr = format!("{}/128", config.ghost_ipv6);
    nsenter_run(
        pid,
        &["ip", "-6", "addr", "add", &ipv6_cidr, "dev", "eth0"],
    )
    .await?;

    let ipv4_cidr = format!("{}/32", config.guest_ipv4);
    nsenter_run(
        pid,
        &["ip", "addr", "add", &ipv4_cidr, "dev", "eth0"],
    )
    .await?;

    // 6. Default IPv6 route via bridge gateway (fe80::1)
    nsenter_run(
        pid,
        &[
            "ip", "-6", "route", "add", "default", "via", "fe80::1", "dev", "eth0",
        ],
    )
    .await?;

    info!(
        "[netns:{}] Pod network configured: eth0={} + {}",
        &config.pod_id[..8.min(config.pod_id.len())],
        config.ghost_ipv6,
        config.guest_ipv4
    );
    Ok(())
}

/// Tear down pod networking by deleting the host-side veth.
/// The peer inside the netns is automatically removed by the kernel.
pub async fn teardown_pod_network(pod_id: &str) {
    let short = &pod_id[..8.min(pod_id.len())];
    let veth_host = format!("veth-{}", short);

    let _ = tokio::process::Command::new("ip")
        .args(["link", "delete", &veth_host])
        .output()
        .await;

    info!("[netns:{}] veth {} removed", short, veth_host);
}
