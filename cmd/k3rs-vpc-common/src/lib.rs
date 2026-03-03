//! Shared types for k3rs-vpc eBPF programs and userspace.
//!
//! All types are `#[repr(C)]` for BPF map compatibility.
//! This crate is `no_std`-compatible for use in eBPF programs.

#![no_std]

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
