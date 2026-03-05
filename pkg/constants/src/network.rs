//! Network-related constants.

/// Default port for the k3rs API server.
pub const DEFAULT_API_PORT: u16 = 6443;

/// Default API server address (HTTP).
pub const DEFAULT_API_ADDR: &str = "http://127.0.0.1:6443";

/// Default tunnel proxy port (agent-side).
pub const DEFAULT_TUNNEL_PORT: u16 = 6444;

/// Default agent API port (registered with the control plane).
pub const DEFAULT_AGENT_API_PORT: u16 = 10250;

/// Default service proxy / kube-proxy port.
pub const DEFAULT_SERVICE_PROXY_PORT: u16 = 10256;

/// Default embedded DNS server port.
pub const DEFAULT_DNS_PORT: u16 = 5353;

/// Well-known DNS virtual IP assigned to the k3rs0 bridge.
/// Pods use this as their nameserver in `/etc/resolv.conf`.
pub const DNS_VIP: &str = "fd6b:3372::53";

// ─── Ghost IPv6 / VPC ──────────────────────────────────────────────

/// ULA prefix encoding "k3rs": fd + 6b337273 → 0xfd6b3372 (truncated to 32 bits).
pub const PLATFORM_PREFIX: u32 = 0xfd6b_3372;

/// Ghost IPv6 address version field (must be 1).
pub const GHOST_VERSION: u8 = 1;

/// VPC ID 0 is reserved (never assigned to user VPCs).
pub const RESERVED_VPC_ID: u16 = 0;

/// Default VPC ID for services without explicit VPC membership.
pub const DEFAULT_VPC_ID: u16 = 1;

/// State store key where the cluster's unique ID is persisted.
pub const CLUSTER_ID_KEY: &str = "/registry/cluster/id";
