//! k3rs-vpc eBPF TC classifier programs for VPC network isolation.
//!
//! Attaches to pod TAP/veth interfaces as TC classifiers.
//! Enforces VPC isolation: only allows traffic within the same VPC
//! or between peered VPCs. Fails open on error (TC_ACT_OK).
//!
//! **IPv4 classifiers** (tc_egress, tc_ingress):
//!   Look up VPC membership in BPF HashMap by guest IPv4 address.
//!
//! **IPv6 classifiers** (tc_egress_v6, tc_ingress_v6):
//!   Extract VPC ID directly from Ghost IPv6 header bytes 10-11 — no map
//!   lookup needed. Only the PEERINGS map is used for cross-VPC decisions.
//!   Includes anti-spoofing: non-Ghost src with Ghost dst is dropped.

#![no_std]
#![no_main]

use aya_ebpf::{
    bindings::TC_ACT_OK,
    macros::{classifier, map},
    maps::HashMap,
    programs::TcContext,
};
use aya_log_ebpf::info;
use k3rs_vpc_common::{
    PeeringKey, PeeringValue, PodKey, PodValue, VpcCidrKey, VpcCidrValue, GHOST_PREFIX,
};
use network_types::eth::{EthHdr, EtherType};
use network_types::ip::Ipv4Hdr;

const TC_ACT_SHOT: i32 = 2;

// ─── IPv6 packet offsets ────────────────────────────────────────
// IPv6 header: 4 (ver/TC/flow) + 2 (payload len) + 1 (next hdr) + 1 (hop limit) = 8 bytes
// Then: 16 bytes src_addr + 16 bytes dst_addr
const IPV6_SRC_ADDR_OFF: usize = EthHdr::LEN + 8;
const IPV6_DST_ADDR_OFF: usize = EthHdr::LEN + 24;

/// BPF HashMap: IPv4 address → VPC membership.
#[map]
static VPC_MEMBERSHIP: HashMap<PodKey, PodValue> = HashMap::with_max_entries(4096, 0);

/// BPF HashMap: VPC ID → CIDR info.
#[map]
static VPC_CIDRS: HashMap<VpcCidrKey, VpcCidrValue> = HashMap::with_max_entries(256, 0);

/// BPF HashMap: (src_vpc, dst_vpc) → peering permission.
#[map]
static PEERINGS: HashMap<PeeringKey, PeeringValue> = HashMap::with_max_entries(1024, 0);

// ─── IPv4 Classifiers (unchanged) ───────────────────────────────

/// TC egress classifier — enforces VPC isolation on outbound IPv4 traffic.
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
            // Destination not a managed pod — allow (traffic to external networks)
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

/// TC ingress classifier — mirror of egress logic for inbound IPv4 traffic.
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

    let src_key = PodKey { ipv4_addr: src_ip };
    let src_pod = unsafe { VPC_MEMBERSHIP.get(&src_key) };
    let src_pod = match src_pod {
        Some(v) => v,
        None => return Ok(TC_ACT_OK),
    };

    let dst_key = PodKey { ipv4_addr: dst_ip };
    let dst_pod = unsafe { VPC_MEMBERSHIP.get(&dst_key) };
    let dst_pod = match dst_pod {
        Some(v) => v,
        None => return Ok(TC_ACT_OK),
    };

    if src_pod.vpc_id == dst_pod.vpc_id {
        return Ok(TC_ACT_OK);
    }

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

// ─── IPv6 Classifiers (Ghost IPv6 native) ───────────────────────

/// Check if 4 bytes at `offset` in the packet match the Ghost IPv6 prefix.
///
/// Reads raw u32 from packet (in network/wire order) and compares with
/// GHOST_PREFIX converted to network byte order.
#[inline(always)]
fn is_ghost_prefix(ctx: &TcContext, addr_offset: usize) -> Result<bool, ()> {
    let prefix_raw: u32 = ctx.load(addr_offset).map_err(|_| ())?;
    // prefix_raw is the raw bytes interpreted as native u32.
    // GHOST_PREFIX is in host byte order (0xfd6b3372).
    // Convert GHOST_PREFIX to big-endian to match the wire format.
    Ok(prefix_raw == GHOST_PREFIX.to_be())
}

/// Extract VPC ID (u16) from bytes 10-11 of an IPv6 address at `addr_offset`.
#[inline(always)]
fn extract_vpc_id(ctx: &TcContext, addr_offset: usize) -> Result<u16, ()> {
    let raw: u16 = ctx.load(addr_offset + 10).map_err(|_| ())?;
    Ok(u16::from_be(raw))
}

/// Shared IPv6 VPC enforcement logic used by both egress and ingress.
///
/// 1. Validate Ghost IPv6 prefix on src and dst addresses.
/// 2. Anti-spoofing: non-Ghost src + Ghost dst → DROP.
/// 3. Extract VPC ID from bytes 10-11 of each Ghost IPv6 address (no map lookup).
/// 4. Same VPC → ALLOW.
/// 5. Different VPC → check PEERINGS map.
/// 6. No peering → DROP.
fn enforce_v6(ctx: &TcContext) -> Result<i32, ()> {
    let eth_hdr: EthHdr = ctx.load(0).map_err(|_| ())?;
    if eth_hdr.ether_type != EtherType::Ipv6 {
        return Ok(TC_ACT_OK);
    }

    let src_is_ghost = is_ghost_prefix(ctx, IPV6_SRC_ADDR_OFF)?;
    let dst_is_ghost = is_ghost_prefix(ctx, IPV6_DST_ADDR_OFF)?;

    // Anti-spoofing: non-Ghost source targeting a Ghost destination → drop.
    // Prevents external/spoofed traffic from reaching Ghost-addressed pods.
    if !src_is_ghost && dst_is_ghost {
        return Ok(TC_ACT_SHOT);
    }

    // If neither is Ghost, or only src is Ghost (pod → external) → pass.
    if !src_is_ghost || !dst_is_ghost {
        return Ok(TC_ACT_OK);
    }

    // Both addresses are Ghost — extract VPC IDs directly from the header.
    // No BPF map lookup needed! VPC ID is encoded at bytes 10-11 of the address.
    let src_vpc_id = extract_vpc_id(ctx, IPV6_SRC_ADDR_OFF)?;
    let dst_vpc_id = extract_vpc_id(ctx, IPV6_DST_ADDR_OFF)?;

    // Same VPC → allow
    if src_vpc_id == dst_vpc_id {
        return Ok(TC_ACT_OK);
    }

    // Different VPC → check peerings map
    let peering_key = PeeringKey {
        src_vpc_id,
        dst_vpc_id,
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

/// TC egress classifier for IPv6 — enforces VPC isolation on outbound Ghost IPv6 traffic.
///
/// Extracts VPC ID directly from Ghost IPv6 header (bytes 10-11) — no BPF map lookup
/// for same-VPC decisions. Only the PEERINGS map is consulted for cross-VPC traffic.
#[classifier]
pub fn tc_egress_v6(ctx: TcContext) -> i32 {
    match enforce_v6(&ctx) {
        Ok(action) => action,
        Err(_) => TC_ACT_OK, // fail-open
    }
}

/// TC ingress classifier for IPv6 — mirror of egress v6 logic for inbound traffic.
#[classifier]
pub fn tc_ingress_v6(ctx: TcContext) -> i32 {
    match enforce_v6(&ctx) {
        Ok(action) => action,
        Err(_) => TC_ACT_OK, // fail-open
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe { core::hint::unreachable_unchecked() }
}
