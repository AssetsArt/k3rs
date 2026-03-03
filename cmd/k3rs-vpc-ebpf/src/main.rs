//! k3rs-vpc eBPF TC classifier programs for VPC network isolation.
//!
//! Attaches to pod TAP/veth interfaces as TC classifiers.
//! Enforces VPC isolation: only allows traffic within the same VPC
//! or between peered VPCs. Fails open on error (TC_ACT_OK).

#![no_std]
#![no_main]

use aya_ebpf::{
    bindings::TC_ACT_OK,
    macros::{classifier, map},
    maps::HashMap,
    programs::TcContext,
};
use aya_log_ebpf::info;
use k3rs_vpc_common::{PeeringKey, PeeringValue, PodKey, PodValue, VpcCidrKey, VpcCidrValue};
use network_types::{
    eth::{EthHdr, EtherType},
    ip::Ipv4Hdr,
};

const TC_ACT_SHOT: i32 = 2;

/// BPF HashMap: IPv4 address → VPC membership.
#[map]
static VPC_MEMBERSHIP: HashMap<PodKey, PodValue> = HashMap::with_max_entries(4096, 0);

/// BPF HashMap: VPC ID → CIDR info.
#[map]
static VPC_CIDRS: HashMap<VpcCidrKey, VpcCidrValue> = HashMap::with_max_entries(256, 0);

/// BPF HashMap: (src_vpc, dst_vpc) → peering permission.
#[map]
static PEERINGS: HashMap<PeeringKey, PeeringValue> = HashMap::with_max_entries(1024, 0);

/// TC egress classifier — enforces VPC isolation on outbound traffic.
///
/// Logic:
/// 1. Parse IPv4 header; pass non-IPv4 traffic.
/// 2. Look up source IP in VPC_MEMBERSHIP → get src_vpc_id.
/// 3. Look up destination IP in VPC_MEMBERSHIP → get dst_vpc_id.
/// 4. If both are in the same VPC → allow.
/// 5. If a peering exists for (src_vpc, dst_vpc) → allow.
/// 6. Otherwise → drop.
/// 7. On any error (missing map entry, parse failure) → allow (fail-open).
#[classifier]
pub fn tc_egress(ctx: TcContext) -> i32 {
    match try_tc_egress(&ctx) {
        Ok(action) => action,
        Err(_) => TC_ACT_OK, // fail-open
    }
}

fn try_tc_egress(ctx: &TcContext) -> Result<i32, ()> {
    let eth_hdr: EthHdr = ctx.load(0).map_err(|_| ())?;
    if eth_hdr.ether_type != EtherType::Ipv4 {
        return Ok(TC_ACT_OK);
    }

    let ipv4_hdr: Ipv4Hdr = ctx.load(EthHdr::LEN).map_err(|_| ())?;
    let src_ip = u32::from_be(ipv4_hdr.src_addr());
    let dst_ip = u32::from_be(ipv4_hdr.dst_addr());

    // Look up source pod
    let src_key = PodKey { ipv4_addr: src_ip };
    let src_pod = unsafe { VPC_MEMBERSHIP.get(&src_key) };
    let src_pod = match src_pod {
        Some(v) => v,
        None => return Ok(TC_ACT_OK), // not a managed pod — pass
    };

    // Look up destination pod
    let dst_key = PodKey { ipv4_addr: dst_ip };
    let dst_pod = unsafe { VPC_MEMBERSHIP.get(&dst_key) };
    let dst_pod = match dst_pod {
        Some(v) => v,
        None => {
            // Destination not a managed pod — check if it's within any VPC CIDR
            // If not managed, allow (traffic to external networks)
            return Ok(TC_ACT_OK);
        }
    };

    // Same VPC → allow
    if src_pod.vpc_id == dst_pod.vpc_id {
        return Ok(TC_ACT_OK);
    }

    // Check peering
    let peering_key = PeeringKey {
        src_vpc_id: src_pod.vpc_id,
        dst_vpc_id: dst_pod.vpc_id,
    };
    let peering = unsafe { PEERINGS.get(&peering_key) };
    if let Some(p) = peering {
        if p.allowed != 0 {
            return Ok(TC_ACT_OK);
        }
    }

    // Different VPC, no peering → drop
    Ok(TC_ACT_SHOT)
}

/// TC ingress classifier — mirror of egress logic for inbound traffic.
#[classifier]
pub fn tc_ingress(ctx: TcContext) -> i32 {
    match try_tc_ingress(&ctx) {
        Ok(action) => action,
        Err(_) => TC_ACT_OK, // fail-open
    }
}

fn try_tc_ingress(ctx: &TcContext) -> Result<i32, ()> {
    let eth_hdr: EthHdr = ctx.load(0).map_err(|_| ())?;
    if eth_hdr.ether_type != EtherType::Ipv4 {
        return Ok(TC_ACT_OK);
    }

    let ipv4_hdr: Ipv4Hdr = ctx.load(EthHdr::LEN).map_err(|_| ())?;
    let src_ip = u32::from_be(ipv4_hdr.src_addr());
    let dst_ip = u32::from_be(ipv4_hdr.dst_addr());

    // Look up source
    let src_key = PodKey { ipv4_addr: src_ip };
    let src_pod = unsafe { VPC_MEMBERSHIP.get(&src_key) };
    let src_pod = match src_pod {
        Some(v) => v,
        None => return Ok(TC_ACT_OK),
    };

    // Look up destination
    let dst_key = PodKey { ipv4_addr: dst_ip };
    let dst_pod = unsafe { VPC_MEMBERSHIP.get(&dst_key) };
    let dst_pod = match dst_pod {
        Some(v) => v,
        None => return Ok(TC_ACT_OK),
    };

    // Same VPC → allow
    if src_pod.vpc_id == dst_pod.vpc_id {
        return Ok(TC_ACT_OK);
    }

    // Check peering (note: for ingress, src is the remote, dst is local)
    let peering_key = PeeringKey {
        src_vpc_id: src_pod.vpc_id,
        dst_vpc_id: dst_pod.vpc_id,
    };
    let peering = unsafe { PEERINGS.get(&peering_key) };
    if let Some(p) = peering {
        if p.allowed != 0 {
            return Ok(TC_ACT_OK);
        }
    }

    Ok(TC_ACT_SHOT)
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe { core::hint::unreachable_unchecked() }
}
