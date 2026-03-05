//! Per-pod netkit setup — creates netkit pairs (L2 mode), assigns Ghost IPv6 +
//! guest IPv4, and configures routes inside the container network namespace.
//!
//! Uses `ip` commands and `nsenter` to configure networking inside the
//! container's network namespace, consistent with the existing Firecracker
//! TAP setup pattern.

use anyhow::Result;
use pkg_constants::network::{
    BRIDGE_GATEWAY_IPV6, DNS_NDOTS, DNS_VIP, GUEST_IFACE, NETKIT_HOST_PREFIX, NETKIT_PEER_PREFIX,
    POD_IPV4_GATEWAY,
};
use tracing::{info, warn};

/// Network configuration for a single pod.
pub struct PodNetworkConfig {
    /// Pod identifier (used to derive netkit device names).
    pub pod_id: String,
    /// Ghost IPv6 address allocated by k3rs-vpc.
    pub ghost_ipv6: String,
    /// Guest IPv4 address for app compatibility.
    pub guest_ipv4: String,
    /// PID of the container's init process (from OCI create --pid-file).
    pub container_pid: u32,
    /// Bridge to attach the host-side netkit to.
    pub bridge_name: String,
}

impl PodNetworkConfig {
    /// Derive the host-side netkit name from the pod ID.
    fn nk_host(&self) -> String {
        let short = &self.pod_id[..8.min(self.pod_id.len())];
        format!("{}{}", NETKIT_HOST_PREFIX, short)
    }

    /// Derive the temporary peer name (moved into the netns, then renamed to GUEST_IFACE).
    fn nk_peer(&self) -> String {
        let short = &self.pod_id[..8.min(self.pod_id.len())];
        format!("{}{}", NETKIT_PEER_PREFIX, short)
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

/// Run a command inside the container's network + mount namespaces via nsenter.
/// Needed for operations that write to the container's filesystem (e.g. /etc/resolv.conf).
async fn nsenter_run_with_mount(pid: u32, args: &[&str]) -> Result<()> {
    let pid_str = pid.to_string();
    let mut cmd_args = vec!["-t", &pid_str, "-n", "-m", "--"];
    cmd_args.extend_from_slice(args);

    let output = tokio::process::Command::new("nsenter")
        .args(&cmd_args)
        .output()
        .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "nsenter -t {} -n -m -- {} failed: {}",
            pid,
            args.join(" "),
            stderr.trim()
        );
    }
    Ok(())
}

/// Create netkit pair (L2 mode), move peer into container netns, attach host
/// side to bridge, and configure IPv6/IPv4 addresses + routes inside the container.
pub async fn setup_pod_network(config: &PodNetworkConfig) -> Result<()> {
    let nk_host = config.nk_host();
    let nk_peer = config.nk_peer();
    let pid = config.container_pid;

    info!(
        "[netns:{}] Setting up pod network: nk={}, ghost_ipv6={}, guest_ipv4={}, pid={}",
        &config.pod_id[..8.min(config.pod_id.len())],
        nk_host,
        config.ghost_ipv6,
        config.guest_ipv4,
        pid
    );

    // 1. Create netkit pair (L2 mode)
    run_ip(&[
        "link", "add", &nk_host, "type", "netkit", "mode", "l2", "peer", "name", &nk_peer,
    ])
    .await?;

    // 2. Move peer into container netns
    let pid_str = pid.to_string();
    run_ip(&["link", "set", &nk_peer, "netns", &pid_str]).await?;

    // 3. Attach host side to bridge and bring up
    run_ip(&["link", "set", &nk_host, "master", &config.bridge_name]).await?;
    run_ip(&["link", "set", &nk_host, "up"]).await?;

    // 4. Inside netns: rename peer → eth0, bring up lo + eth0
    nsenter_run(pid, &["ip", "link", "set", &nk_peer, "name", GUEST_IFACE]).await?;
    nsenter_run(pid, &["ip", "link", "set", "lo", "up"]).await?;
    nsenter_run(pid, &["ip", "link", "set", GUEST_IFACE, "up"]).await?;

    // 5. Assign Ghost IPv6 (/128) and guest IPv4 (/32) to eth0
    let ipv6_cidr = format!("{}/128", config.ghost_ipv6);
    nsenter_run(pid, &["ip", "-6", "addr", "add", &ipv6_cidr, "dev", GUEST_IFACE, "nodad"]).await?;

    let ipv4_cidr = format!("{}/32", config.guest_ipv4);
    nsenter_run(pid, &["ip", "addr", "add", &ipv4_cidr, "dev", GUEST_IFACE]).await?;

    // 6. Default IPv6 route via bridge gateway
    nsenter_run(
        pid,
        &[
            "ip", "-6", "route", "add", "default", "via", BRIDGE_GATEWAY_IPV6, "dev", GUEST_IFACE,
        ],
    )
    .await?;

    // 7. IPv4 default route via link-local gateway (SIIT on host-side netkit)
    //    Add 169.254.1.1 as a directly-connected next-hop, then use it as default gw.
    let gw_cidr = format!("{}/32", POD_IPV4_GATEWAY);
    nsenter_run(
        pid,
        &[
            "ip",
            "route",
            "add",
            &gw_cidr,
            "dev",
            GUEST_IFACE,
            "scope",
            "link",
        ],
    )
    .await?;
    nsenter_run(
        pid,
        &[
            "ip",
            "route",
            "add",
            "default",
            "via",
            POD_IPV4_GATEWAY,
            "dev",
            GUEST_IFACE,
        ],
    )
    .await?;

    // 8. Enable proxy_arp on host-side netkit so it responds to ARP for 169.254.1.1
    let proxy_arp_path = format!("/proc/sys/net/ipv4/conf/{}/proxy_arp", config.nk_host());
    if let Err(e) = tokio::fs::write(&proxy_arp_path, "1").await {
        warn!(
            "[netns:{}] Failed to enable proxy_arp on {}: {}",
            &config.pod_id[..8.min(config.pod_id.len())],
            config.nk_host(),
            e
        );
    }

    // 9. Write /etc/resolv.conf pointing to the k3rs DNS VIP on the bridge
    //    Uses mount namespace (-m) so we write to the container's filesystem, not the host's.
    let resolv_content = format!("nameserver {}\noptions ndots:{}\n", DNS_VIP, DNS_NDOTS);
    if let Err(e) = nsenter_run_with_mount(
        pid,
        &["sh", "-c", &format!("echo '{}' > /etc/resolv.conf", resolv_content.trim())],
    )
    .await
    {
        warn!(
            "[netns:{}] Failed to write resolv.conf: {}",
            &config.pod_id[..8.min(config.pod_id.len())],
            e
        );
    }

    info!(
        "[netns:{}] Pod network configured: eth0={} + {}, dns={}",
        &config.pod_id[..8.min(config.pod_id.len())],
        config.ghost_ipv6,
        config.guest_ipv4,
        DNS_VIP
    );
    Ok(())
}

/// Tear down pod networking by deleting the host-side netkit device.
/// The peer inside the netns is automatically removed by the kernel.
pub async fn teardown_pod_network(pod_id: &str) {
    let short = &pod_id[..8.min(pod_id.len())];
    let nk_host = format!("{}{}", NETKIT_HOST_PREFIX, short);

    let _ = tokio::process::Command::new("ip")
        .args(["link", "delete", &nk_host])
        .output()
        .await;

    info!("[netns:{}] netkit {} removed", short, nk_host);
}
