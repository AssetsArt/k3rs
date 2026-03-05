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

// ─── Cluster networking ─────────────────────────────────────────

/// Network bridge interface name for pod-to-pod traffic.
pub const BRIDGE_NAME: &str = "k3rs0";

/// Link-local IPv6 gateway assigned to the bridge.
pub const BRIDGE_GATEWAY_IPV6: &str = "fe80::1";

/// Cluster IP subnet prefix for Service VIPs.
pub const CLUSTER_IP_PREFIX: &str = "10.43.0.";

/// Default VPC name for resources without explicit VPC membership.
pub const DEFAULT_VPC_NAME: &str = "default";

/// Namespaces seeded on cluster bootstrap.
pub const SEED_NAMESPACES: &[&str] = &["default", "k3rs-system"];

/// Local container registry exception (plain HTTP allowed).
pub const LOCAL_REGISTRY: &str = "localhost:5000";

/// Default OpenTelemetry OTLP gRPC endpoint.
pub const DEFAULT_OTEL_ENDPOINT: &str = "http://localhost:4317";

/// GitHub repository for binary releases.
pub const GITHUB_REPO: &str = "AssetsArt/k3rs";

// ─── Pod netns ────────────────────────────────────────────────────

/// Interface name assigned inside the container network namespace.
pub const GUEST_IFACE: &str = "eth0";

/// Netkit host-side device name prefix (followed by truncated pod ID).
pub const NETKIT_HOST_PREFIX: &str = "nk-";

/// Netkit temporary peer name prefix (moved into netns, then renamed to GUEST_IFACE).
pub const NETKIT_PEER_PREFIX: &str = "nktmp-";

/// Link-local IPv4 gateway used inside pod netns for NAT64 / SIIT routing.
pub const POD_IPV4_GATEWAY: &str = "169.254.1.1";

/// DNS `ndots` option written to pod `/etc/resolv.conf`.
pub const DNS_NDOTS: u8 = 5;

// ─── WireGuard mesh ───────────────────────────────────────────────

/// Default WireGuard interface name.
pub const WG_INTERFACE: &str = "wg-k3rs";

/// Default WireGuard listen port.
pub const WG_DEFAULT_PORT: u16 = 51820;

/// Default path for WireGuard key storage.
pub const WG_DEFAULT_KEY_PATH: &str = "/var/lib/k3rs/wireguard";

/// The Ghost IPv6 prefix route added to the WireGuard interface.
/// Covers all pods on all nodes in all VPCs (fd6b:3372::/32).
pub const GHOST_ROUTE_PREFIX: &str = "fd6b:3372::/32";
