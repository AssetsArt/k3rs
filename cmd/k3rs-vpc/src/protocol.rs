//! NDJSON request/response types for the VPC daemon Unix socket protocol (§9.4).

use pkg_types::vpc::VpcStatus;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum VpcRequest {
    Allocate { pod_id: String, vpc_name: String },
    Release { pod_id: String, vpc_name: String },
    Query { pod_id: String },
    GetRoutes { vpc_id: u16 },
    CheckReachability { src_vpc: String, dst_vpc: String },
    /// Attach SIIT translators and IPv6 VPC classifiers to a veth interface.
    AttachVeth {
        veth_name: String,
        guest_ipv4: String,
        ghost_ipv6: String,
        vpc_id: u16,
    },
    /// Detach TC classifiers from a veth interface.
    DetachVeth { veth_name: String },
    ListVpcs,
    Ping,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum VpcResponse {
    Allocated {
        guest_ipv4: String,
        ghost_ipv6: String,
        vpc_id: u16,
    },
    Released,
    QueryResult {
        guest_ipv4: String,
        ghost_ipv6: String,
        vpc_id: u16,
        vpc_name: String,
    },
    Routes {
        entries: Vec<RouteEntry>,
    },
    Reachable {
        reachable: bool,
    },
    VpcList {
        vpcs: Vec<VpcInfo>,
    },
    Pong,
    /// Acknowledgement for AttachVeth / DetachVeth.
    Ok,
    Error {
        code: String,
        message: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RouteEntry {
    pub destination: String,
    pub next_hop: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VpcInfo {
    pub name: String,
    pub vpc_id: u16,
    pub ipv4_cidr: String,
    pub status: VpcStatus,
}
