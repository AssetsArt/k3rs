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
