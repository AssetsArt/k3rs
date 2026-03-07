//! Linux capabilities required by each k3rs component.
//!
//! These are used by the process manager to:
//! - Generate systemd service files with `AmbientCapabilities=`
//! - Apply file capabilities via `setcap` for non-systemd usage

/// Capabilities for k3rs-server.
/// Server only binds to port 6443 and manages state — no networking.
pub const SERVER_CAPS: &[&str] = &["CAP_NET_BIND_SERVICE"];

/// Capabilities for k3rs-agent.
/// Needs to create bridges, netkit pairs, netns config, WireGuard, write sysctl.
pub const AGENT_CAPS: &[&str] = &[
    "CAP_NET_ADMIN",    // ip link, ip route, bridge, wireguard
    "CAP_NET_RAW",      // raw sockets (health checks, ARP)
    "CAP_SYS_ADMIN",    // nsenter, mount namespaces, cgroup, sysctl writes
    "CAP_SYS_PTRACE",   // nsenter -t PID -n (enter other process's netns)
    "CAP_DAC_OVERRIDE", // write /etc/resolv.conf inside container rootfs
];

/// Capabilities for k3rs-vpc.
/// eBPF enforcement, NAT64, reads default route.
pub const VPC_CAPS: &[&str] = &[
    "CAP_NET_ADMIN", // ip route, iptables, NAT64
    "CAP_BPF",       // load/attach eBPF programs
    "CAP_SYS_ADMIN", // pinned BPF maps, sysctl
    "CAP_PERFMON",   // eBPF perf events
];

/// Capabilities for k3rs-ui.
/// Dashboard only serves HTTP — no special privileges.
pub const UI_CAPS: &[&str] = &[];

/// Get capabilities for a component by its registry key.
pub fn caps_for_component(key: &str) -> &'static [&'static str] {
    match key {
        "server" => SERVER_CAPS,
        "agent" => AGENT_CAPS,
        "vpc" => VPC_CAPS,
        "ui" => UI_CAPS,
        _ => &[],
    }
}
