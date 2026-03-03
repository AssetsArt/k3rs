//! Shared types for k3rs-vpc eBPF programs and userspace.
//!
//! All types are `#[repr(C)]` for BPF map compatibility.
//! This crate is `no_std`-compatible for use in eBPF programs.

#![no_std]

// ─── Ghost IPv6 Constants ───────────────────────────────────────

/// Ghost IPv6 platform prefix in host byte order: `fd6b:3372` (ULA "k3rs").
///
/// In the packet, the first 4 bytes of a Ghost IPv6 address are `[0xfd, 0x6b, 0x33, 0x72]`.
/// Use `GHOST_PREFIX.to_be()` to get the network byte order value for raw packet comparison.
pub const GHOST_PREFIX: u32 = 0xfd6b_3372;

/// Offset (in bytes) of the VPC ID within a Ghost IPv6 address.
/// VPC ID is a big-endian u16 at bytes 10-11 of the 16-byte address.
pub const GHOST_VPC_ID_OFFSET: usize = 10;

// ─── BPF Map Types ──────────────────────────────────────────────

/// Key for the VPC_MEMBERSHIP BPF hash map.
/// Maps a pod's IPv4 address to its VPC membership.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PodKey {
    /// Pod's guest IPv4 address in network byte order.
    pub ipv4_addr: u32,
}

/// Value for the VPC_MEMBERSHIP BPF hash map.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PodValue {
    /// VPC ID this pod belongs to.
    pub vpc_id: u16,
    /// Padding for alignment.
    pub _pad: u16,
}

/// Key for the VPC_CIDRS BPF hash map.
/// Maps a VPC ID to its CIDR information.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VpcCidrKey {
    /// VPC ID.
    pub vpc_id: u16,
    /// Padding for alignment.
    pub _pad: u16,
}

/// Value for the VPC_CIDRS BPF hash map.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VpcCidrValue {
    /// Network address in network byte order (e.g. 10.0.0.0).
    pub network: u32,
    /// Network mask in network byte order (e.g. 0xFFFFFF00 for /24).
    pub mask: u32,
}

/// Key for the PEERINGS BPF hash map.
/// Encodes a directional peering relationship between two VPCs.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeeringKey {
    /// Source VPC ID.
    pub src_vpc_id: u16,
    /// Destination VPC ID.
    pub dst_vpc_id: u16,
}

/// Value for the PEERINGS BPF hash map.
/// A non-zero `allowed` means traffic is permitted.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeeringValue {
    /// 1 = allowed, 0 = denied.
    pub allowed: u32,
}

// ─── NAT64 BPF Map Types ────────────────────────────────────────

/// NAT64 well-known prefix first 4 bytes: `0064:ff9b` (RFC 6052).
/// Use `NAT64_PREFIX_U32.to_be()` for network byte order comparison.
pub const NAT64_PREFIX_U32: u32 = 0x0064_ff9b;

/// Key for the NAT64_CONNTRACK LRU hash map.
/// Identifies a translated outbound flow for return-path matching.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Nat64Key {
    /// Transport protocol (TCP=6, UDP=17).
    pub protocol: u8,
    pub _pad: u8,
    /// Source port from the pod (kept through SNAT).
    pub src_port: u16,
    /// Destination IPv4 extracted from `64:ff9b::x` (host byte order).
    pub dst_ipv4: u32,
    /// Destination port.
    pub dst_port: u16,
    pub _pad2: u16,
}

/// Value for the NAT64_CONNTRACK LRU hash map.
/// Stores the pod's Ghost IPv6 source for reverse translation.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Nat64Value {
    /// Original Ghost IPv6 source address (16 bytes).
    pub src_ipv6: [u8; 16],
}

/// Configuration for the NAT64 eBPF programs (BPF array, index 0).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Nat64Config {
    /// Node's external IPv4 address for SNAT (host byte order).
    pub node_ipv4: u32,
    /// Physical/external interface index (for redirect after IPv6→IPv4).
    pub phys_ifindex: u32,
    /// k3rs0 bridge interface index (for redirect after IPv4→IPv6).
    pub bridge_ifindex: u32,
    pub _pad: u32,
}
