//! DNS-related constants.

/// DNS query type: A record (IPv4).
pub const QTYPE_A: u16 = 1;

/// DNS query type: AAAA record (IPv6).
pub const QTYPE_AAAA: u16 = 28;

/// NAT64 well-known prefix (first 12 bytes): `64:ff9b::/96` (RFC 6052).
pub const NAT64_PREFIX: [u8; 12] = [0x00, 0x64, 0xff, 0x9b, 0, 0, 0, 0, 0, 0, 0, 0];

/// Default DNS domain suffix for internal service discovery.
pub const DNS_DOMAIN_SUFFIX: &str = "svc.cluster.local";

/// Default upstream DNS resolver address.
pub const DEFAULT_UPSTREAM_DNS: &str = "8.8.8.8:53";

/// Timeout for forwarding queries to the upstream DNS resolver.
pub const UPSTREAM_DNS_TIMEOUT_SECS: u64 = 2;
