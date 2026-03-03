//! k3rs-vpc eBPF TC classifier programs for VPC network isolation and NAT64.
//!
//! **VPC classifiers** — attach to pod TAP/netkit interfaces:
//!   IPv4 (tc_egress, tc_ingress): Per-pod .rodata + VPC_PODS for same-VPC, VPC_MEMBERSHIP fallback for peering.
//!   IPv6 (tc_egress_v6, tc_ingress_v6): VPC ID from Ghost IPv6 bytes 10-11.
//!
//! **NAT64** — attach to k3rs0 bridge + physical interface:
//!   nat64_egress: IPv6 `64:ff9b::x` → IPv4, SNAT to node IPv4, redirect to phys.
//!   nat64_ingress: IPv4 return → IPv6, reverse SNAT, redirect to bridge.

#![no_std]
#![no_main]

use aya_ebpf::{
    bindings::TC_ACT_OK,
    macros::{classifier, map},
    maps::{Array, HashMap, LruHashMap},
    programs::TcContext,
};
use aya_ebpf_bindings::helpers::bpf_redirect;
use k3rs_vpc_common::{
    Nat64Config, Nat64Key, Nat64Value, PeeringKey, PeeringValue, PodKey, PodValue, VpcCidrKey,
    VpcCidrValue, VpcPodKey, VpcPodValue, GHOST_PREFIX, NAT64_PREFIX_U32,
};
use network_types::eth::EthHdr;

const TC_ACT_SHOT: i32 = 2;
const ETH_P_IP: u16 = 0x0800;
const ETH_P_IPV6: u16 = 0x86DD;

// ─── IPv6 packet offsets ────────────────────────────────────────
// IPv6 header: 4 (ver/TC/flow) + 2 (payload len) + 1 (next hdr) + 1 (hop limit) = 8 bytes
// Then: 16 bytes src_addr + 16 bytes dst_addr
const IPV6_SRC_ADDR_OFF: usize = EthHdr::LEN + 8;
const IPV6_DST_ADDR_OFF: usize = EthHdr::LEN + 24;

// ─── Per-pod .rodata globals (baked in at load time via EbpfLoader::set_global) ──
#[no_mangle]
static MY_GHOST_IPV6: [u8; 16] = [0u8; 16];
#[no_mangle]
static MY_GUEST_IPV4: u32 = 0;
#[no_mangle]
static MY_VPC_ID: u16 = 0;
#[no_mangle]
static MY_VPC_NETWORK: u32 = 0; // VPC network in host byte order (e.g. 0x0a000100 for 10.0.1.0)
#[no_mangle]
static MY_VPC_MASK: u32 = 0; // VPC mask in host byte order (e.g. 0xffffff00 for /24)

/// BPF HashMap: IPv4 address → VPC membership (pinned, shared across per-pod instances).
#[map]
static VPC_MEMBERSHIP: HashMap<PodKey, PodValue> = HashMap::pinned(4096, 0);

/// BPF HashMap: VPC ID → CIDR info (pinned, shared).
#[map]
static VPC_CIDRS: HashMap<VpcCidrKey, VpcCidrValue> = HashMap::pinned(256, 0);

/// BPF HashMap: (src_vpc, dst_vpc) → peering permission (pinned, shared).
#[map]
static PEERINGS: HashMap<PeeringKey, PeeringValue> = HashMap::pinned(1024, 0);

// ─── IPv4 Classifiers (unchanged) ───────────────────────────────

/// TC egress classifier — enforces VPC isolation on outbound IPv4 traffic.
///
/// Attached to TAP interface egress (host → VM). Uses per-pod .rodata globals
/// for VPC identity. Primary same-VPC check uses VPC_PODS (VPC-scoped key,
/// no collision with overlapping CIDRs). VPC_MEMBERSHIP is a fallback for
/// cross-VPC peering decisions only.
///
/// Logic:
/// 1. Parse IPv4 header; pass non-IPv4 traffic.
/// 2. Read MY_VPC_ID from .rodata (baked at load time).
/// 3. Same-VPC: VPC_PODS[(MY_VPC_ID, src_ip)] exists → allow.
/// 4. Cross-VPC: VPC_MEMBERSHIP[src_ip] → src_vpc_id → peering check.
/// 5. External (not in VPC_MEMBERSHIP) → allow.
/// 6. On any error → allow (fail-open).
#[classifier]
pub fn tc_egress(ctx: TcContext) -> i32 {
    match try_tc_egress(&ctx) {
        Ok(action) => action,
        Err(_) => TC_ACT_OK, // fail-open
    }
}

fn try_tc_egress(ctx: &TcContext) -> Result<i32, ()> {
    let etype: u16 = ctx.load(12).map_err(|_| ())?;
    if etype != ETH_P_IP.to_be() {
        return Ok(TC_ACT_OK);
    }

    let src_ip = u32::from_be(ctx.load::<u32>(EthHdr::LEN + 12).map_err(|_| ())?);

    let my_vpc_id = unsafe { core::ptr::read_volatile(&MY_VPC_ID) };

    // Same-VPC check via VPC_PODS (VPC-scoped key, no collision with overlapping CIDRs)
    let vpc_key = VpcPodKey {
        vpc_id: my_vpc_id,
        _pad: 0,
        ipv4_addr: src_ip,
    };
    if unsafe { VPC_PODS.get(&vpc_key) }.is_some() {
        return Ok(TC_ACT_OK);
    }

    // Cross-VPC: look up src in flat VPC_MEMBERSHIP (for peering decisions)
    let src_key = PodKey { ipv4_addr: src_ip };
    match unsafe { VPC_MEMBERSHIP.get(&src_key) } {
        None => Ok(TC_ACT_OK), // not a managed pod → external → allow
        Some(src_pod) => {
            // Same VPC via VPC_MEMBERSHIP (timing fallback before VPC_PODS is populated)
            if src_pod.vpc_id == my_vpc_id {
                return Ok(TC_ACT_OK);
            }
            // Different VPC → check peering (src_vpc → MY_VPC_ID)
            let peering_key = PeeringKey {
                src_vpc_id: src_pod.vpc_id,
                dst_vpc_id: my_vpc_id,
            };
            match unsafe { PEERINGS.get(&peering_key) } {
                Some(p) if p.allowed != 0 => Ok(TC_ACT_OK),
                _ => Ok(TC_ACT_SHOT),
            }
        }
    }
}

/// TC ingress classifier — enforces VPC isolation on inbound IPv4 traffic.
///
/// Attached to TAP interface ingress (VM → host). Uses per-pod .rodata globals.
/// Anti-spoofs source IP, then checks if destination is reachable from this pod's VPC.
#[classifier]
pub fn tc_ingress(ctx: TcContext) -> i32 {
    match try_tc_ingress(&ctx) {
        Ok(action) => action,
        Err(_) => TC_ACT_OK, // fail-open
    }
}

fn try_tc_ingress(ctx: &TcContext) -> Result<i32, ()> {
    let etype: u16 = ctx.load(12).map_err(|_| ())?;
    if etype != ETH_P_IP.to_be() {
        return Ok(TC_ACT_OK);
    }

    let src_ip = u32::from_be(ctx.load::<u32>(EthHdr::LEN + 12).map_err(|_| ())?);
    let dst_ip = u32::from_be(ctx.load::<u32>(EthHdr::LEN + 16).map_err(|_| ())?);

    let my_vpc_id = unsafe { core::ptr::read_volatile(&MY_VPC_ID) };
    let my_guest_ipv4 = unsafe { core::ptr::read_volatile(&MY_GUEST_IPV4) };

    // Anti-spoof: source must be this pod's guest IPv4
    if src_ip != my_guest_ipv4 {
        return Ok(TC_ACT_SHOT);
    }

    // Same-VPC check via VPC_PODS (VPC-scoped key, no collision)
    let vpc_key = VpcPodKey {
        vpc_id: my_vpc_id,
        _pad: 0,
        ipv4_addr: dst_ip,
    };
    if unsafe { VPC_PODS.get(&vpc_key) }.is_some() {
        return Ok(TC_ACT_OK);
    }

    // Cross-VPC: look up dst in flat VPC_MEMBERSHIP (for peering decisions)
    let dst_key = PodKey { ipv4_addr: dst_ip };
    match unsafe { VPC_MEMBERSHIP.get(&dst_key) } {
        None => Ok(TC_ACT_OK), // not a managed pod → external → allow
        Some(dst_pod) => {
            // Same VPC via VPC_MEMBERSHIP (timing fallback)
            if dst_pod.vpc_id == my_vpc_id {
                return Ok(TC_ACT_OK);
            }
            // Different VPC → check peering (MY_VPC_ID → dst_vpc)
            let peering_key = PeeringKey {
                src_vpc_id: my_vpc_id,
                dst_vpc_id: dst_pod.vpc_id,
            };
            match unsafe { PEERINGS.get(&peering_key) } {
                Some(p) if p.allowed != 0 => Ok(TC_ACT_OK),
                _ => Ok(TC_ACT_SHOT),
            }
        }
    }
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
    let etype: u16 = ctx.load(12).map_err(|_| ())?;
    if etype != ETH_P_IPV6.to_be() {
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

// ─── SIIT Per-Pod Translation Programs ─────────────────────────

/// VPC-scoped pod lookup: (vpc_id, ipv4) → ghost_ipv6 (for intra-VPC routing, pinned, shared).
#[map]
static VPC_PODS: HashMap<VpcPodKey, VpcPodValue> = HashMap::pinned(4096, 0);

/// SIIT inbound translator (IPv4 → IPv6).
/// Attached inside the pod's netns on eth0 TC **Egress** (app sends IPv4 out).
/// Translates IPv4 packets from the pod application → IPv6 before they exit to the host.
///
/// Pod identity (ghost_ipv6, guest_ipv4, vpc_id) is baked into .rodata globals
/// at load time — no map lookup needed.
///
/// 1. Check IPv4, pass non-IPv4.
/// 2. Read pod config from .rodata globals.
/// 3. Anti-spoof: src_ipv4 must match guest_ipv4.
/// 4. Resolve dst IPv6: intra-VPC (VPC_PODS) or external (64:ff9b::).
/// 5. Translate: change_proto(IPv6), write IPv6 header, adjust L4 checksum.
#[classifier]
pub fn siit_in(mut ctx: TcContext) -> i32 {
    match try_siit_in(&mut ctx) {
        Ok(action) => action,
        Err(_) => TC_ACT_OK, // fail-open
    }
}

fn try_siit_in(ctx: &mut TcContext) -> Result<i32, ()> {
    // 1. Check IPv4 ethertype
    let etype: u16 = ctx.load(12).map_err(|_| ())?;
    if etype != ETH_P_IP.to_be() {
        return Ok(TC_ACT_OK);
    }

    // Only handle standard IPv4 headers (IHL=5, no options)
    let ver_ihl: u8 = ctx.load(EthHdr::LEN).map_err(|_| ())?;
    if ver_ihl != 0x45 {
        return Ok(TC_ACT_OK);
    }

    // 2. Read pod config from .rodata globals (baked in at load time)
    let ghost_ipv6 = unsafe { core::ptr::read_volatile(&MY_GHOST_IPV6) };
    let guest_ipv4 = unsafe { core::ptr::read_volatile(&MY_GUEST_IPV4) };
    let vpc_id = unsafe { core::ptr::read_volatile(&MY_VPC_ID) };

    // 3. Read IPv4 header fields
    let protocol: u8 = ctx.load(EthHdr::LEN + 9).map_err(|_| ())?;
    // Only TCP/UDP
    if protocol != IPPROTO_TCP && protocol != IPPROTO_UDP {
        return Ok(TC_ACT_OK);
    }

    let src_raw: u32 = ctx.load(EthHdr::LEN + 12).map_err(|_| ())?;
    let dst_raw: u32 = ctx.load(EthHdr::LEN + 16).map_err(|_| ())?;
    let src_ip = u32::from_be(src_raw);
    let dst_ip = u32::from_be(dst_raw);

    // 4. Anti-spoof: source must be this pod's guest IPv4
    if src_ip != guest_ipv4 {
        return Ok(TC_ACT_SHOT);
    }

    // 5. Resolve destination IPv6
    let dst_ipv6 = {
        let vpc_key = VpcPodKey {
            vpc_id,
            _pad: 0,
            ipv4_addr: dst_ip,
        };
        match unsafe { VPC_PODS.get(&vpc_key) } {
            Some(pod) => pod.ghost_ipv6,
            None => {
                // Formula fallback: if dst is within VPC CIDR, compute Ghost IPv6
                // from this pod's prefix (bytes 0-11) + dst IPv4 (bytes 12-15).
                // Used by VMs where VPC_PODS is empty (separate kernel).
                let vpc_network = unsafe { core::ptr::read_volatile(&MY_VPC_NETWORK) };
                let vpc_mask = unsafe { core::ptr::read_volatile(&MY_VPC_MASK) };
                if vpc_network != 0 && (dst_ip & vpc_mask) == (vpc_network & vpc_mask) {
                    let dst_bytes = dst_ip.to_be_bytes();
                    let mut addr = ghost_ipv6;
                    addr[12] = dst_bytes[0];
                    addr[13] = dst_bytes[1];
                    addr[14] = dst_bytes[2];
                    addr[15] = dst_bytes[3];
                    addr
                } else {
                    nat64_addr(dst_ip) // external: 64:ff9b::dst_ipv4
                }
            }
        }
    };

    // 6. Source IPv6 = pod's Ghost IPv6
    let src_ipv6 = ghost_ipv6;

    // 7. Read remaining IPv4 fields before change_proto
    let tot_len = u16::from_be(ctx.load::<u16>(EthHdr::LEN + 2).map_err(|_| ())?);
    let payload_len = tot_len - IPV4_HDR_LEN as u16;
    let ttl: u8 = ctx.load(EthHdr::LEN + 8).map_err(|_| ())?;
    if ttl <= 1 {
        return Ok(TC_ACT_SHOT); // TTL expired
    }

    // Read L4 ports (for checksum offset)
    let l4 = EthHdr::LEN + IPV4_HDR_LEN;
    let _src_port: u16 = ctx.load(l4).map_err(|_| ())?;

    // 8. Change protocol: IPv4 → IPv6 (grows L3 header by 20 bytes)
    ctx.change_proto(ETH_P_IPV6, 0).map_err(|_| ())?;

    // 9. Write IPv6 header
    let l3 = EthHdr::LEN;
    let s6 = addr_words(&src_ipv6);
    let d6 = addr_words(&dst_ipv6);

    ctx.store(l3, &0x60000000u32.to_be(), 0).map_err(|_| ())?; // ver=6, TC=0, flow=0
    ctx.store(l3 + 4, &payload_len.to_be(), 0).map_err(|_| ())?;
    ctx.store(l3 + 6, &protocol, 0).map_err(|_| ())?; // next header
    ctx.store(l3 + 7, &(ttl - 1), 0).map_err(|_| ())?; // hop limit = TTL-1
    // src IPv6
    ctx.store(l3 + 8, &s6[0], 0).map_err(|_| ())?;
    ctx.store(l3 + 12, &s6[1], 0).map_err(|_| ())?;
    ctx.store(l3 + 16, &s6[2], 0).map_err(|_| ())?;
    ctx.store(l3 + 20, &s6[3], 0).map_err(|_| ())?;
    // dst IPv6
    ctx.store(l3 + 24, &d6[0], 0).map_err(|_| ())?;
    ctx.store(l3 + 28, &d6[1], 0).map_err(|_| ())?;
    ctx.store(l3 + 32, &d6[2], 0).map_err(|_| ())?;
    ctx.store(l3 + 36, &d6[3], 0).map_err(|_| ())?;

    // 10. Adjust L4 checksum (IPv4 pseudo → IPv6 pseudo)
    let csum_off = EthHdr::LEN + IPV6_HDR_LEN
        + if protocol == IPPROTO_TCP { TCP_CSUM_OFF } else { UDP_CSUM_OFF };
    csum_4to6(ctx, csum_off, src_raw, dst_raw, s6, d6)?;

    Ok(TC_ACT_OK)
}

/// SIIT outbound translator (IPv6 → IPv4).
/// Attached inside the pod's netns on eth0 TC **Ingress** (IPv6 arrives from host,
/// deliver IPv4 to app).
///
/// Pod identity is baked into .rodata globals at load time.
///
/// 1. Check IPv6, pass non-IPv6.
/// 2. Read pod config from .rodata globals.
/// 3. Verify dst IPv6 matches this pod's Ghost IPv6.
/// 4. Extract src IPv4 from bytes 12-15 of src IPv6.
/// 5. Translate: change_proto(IPv4), write IPv4 header, adjust L4 checksum.
#[classifier]
pub fn siit_out(mut ctx: TcContext) -> i32 {
    match try_siit_out(&mut ctx) {
        Ok(action) => action,
        Err(_) => TC_ACT_OK, // fail-open
    }
}

fn try_siit_out(ctx: &mut TcContext) -> Result<i32, ()> {
    // 1. Check IPv6 ethertype
    let etype: u16 = ctx.load(12).map_err(|_| ())?;
    if etype != ETH_P_IPV6.to_be() {
        return Ok(TC_ACT_OK);
    }

    // 2. Read pod config from .rodata globals (baked in at load time)
    let ghost_ipv6 = unsafe { core::ptr::read_volatile(&MY_GHOST_IPV6) };
    let guest_ipv4 = unsafe { core::ptr::read_volatile(&MY_GUEST_IPV4) };

    // 3. Read dst IPv6 — verify it matches this pod's Ghost IPv6
    let d0: u32 = ctx.load(IPV6_DST_ADDR_OFF).map_err(|_| ())?;
    let d1: u32 = ctx.load(IPV6_DST_ADDR_OFF + 4).map_err(|_| ())?;
    let d2: u32 = ctx.load(IPV6_DST_ADDR_OFF + 8).map_err(|_| ())?;
    let d3: u32 = ctx.load(IPV6_DST_ADDR_OFF + 12).map_err(|_| ())?;
    let dst_v6_words = [d0, d1, d2, d3];
    let expected = addr_words(&ghost_ipv6);
    if dst_v6_words != expected {
        return Ok(TC_ACT_OK); // not for this pod — pass through
    }

    // 4. Read IPv6 header fields
    let payload_len = u16::from_be(ctx.load::<u16>(EthHdr::LEN + 4).map_err(|_| ())?);
    let next_hdr: u8 = ctx.load(EthHdr::LEN + 6).map_err(|_| ())?;
    let hop_limit: u8 = ctx.load(EthHdr::LEN + 7).map_err(|_| ())?;

    // Only TCP/UDP
    if next_hdr != IPPROTO_TCP && next_hdr != IPPROTO_UDP {
        return Ok(TC_ACT_OK);
    }

    // 5. Read src IPv6 for checksum and IPv4 extraction
    let s0: u32 = ctx.load(IPV6_SRC_ADDR_OFF).map_err(|_| ())?;
    let s1: u32 = ctx.load(IPV6_SRC_ADDR_OFF + 4).map_err(|_| ())?;
    let s2: u32 = ctx.load(IPV6_SRC_ADDR_OFF + 8).map_err(|_| ())?;
    let s3: u32 = ctx.load(IPV6_SRC_ADDR_OFF + 12).map_err(|_| ())?;
    let src_v6 = [s0, s1, s2, s3];

    // src IPv4 = bytes 12-15 of src IPv6 (works for Ghost and NAT64 64:ff9b::)
    let src_ipv4 = u32::from_be(s3);
    let dst_ipv4 = guest_ipv4;

    // 6. Change protocol: IPv6 → IPv4 (shrinks L3 header by 20 bytes)
    ctx.change_proto(ETH_P_IP, 0).map_err(|_| ())?;

    // 7. Write IPv4 header
    let l3 = EthHdr::LEN;
    let total_len = payload_len + IPV4_HDR_LEN as u16;
    ctx.store(l3, &0x45u8, 0).map_err(|_| ())?; // version=4, IHL=5
    ctx.store(l3 + 1, &0u8, 0).map_err(|_| ())?; // DSCP/ECN
    ctx.store(l3 + 2, &total_len.to_be(), 0).map_err(|_| ())?;
    ctx.store(l3 + 4, &0u16, 0).map_err(|_| ())?; // identification
    ctx.store(l3 + 6, &0x4000u16.to_be(), 0).map_err(|_| ())?; // DF
    ctx.store(l3 + 8, &hop_limit, 0).map_err(|_| ())?; // TTL
    ctx.store(l3 + 9, &next_hdr, 0).map_err(|_| ())?; // protocol
    ctx.store(l3 + 10, &0u16, 0).map_err(|_| ())?; // checksum placeholder
    let src_raw = src_ipv4.to_be();
    let dst_raw = dst_ipv4.to_be();
    ctx.store(l3 + 12, &src_raw, 0).map_err(|_| ())?;
    ctx.store(l3 + 16, &dst_raw, 0).map_err(|_| ())?;

    // 8. Compute and store IPv4 header checksum
    let csum = ipv4_hdr_csum(total_len, hop_limit, next_hdr, src_ipv4, dst_ipv4);
    ctx.store(l3 + 10, &csum.to_be(), 0).map_err(|_| ())?;

    // 9. Adjust L4 checksum (IPv6 pseudo → IPv4 pseudo)
    let csum_off = EthHdr::LEN + IPV4_HDR_LEN
        + if next_hdr == IPPROTO_TCP { TCP_CSUM_OFF } else { UDP_CSUM_OFF };
    csum_6to4(ctx, csum_off, src_v6, dst_v6_words, src_raw, dst_raw)?;

    Ok(TC_ACT_OK)
}

// ─── TAP Guard (anti-spoofing for host-side TAP ingress) ────────

/// Anti-spoofing classifier for host-side TAP ingress (VM → host).
/// Validates that only IPv6 traffic exits the VM, and that Ghost-prefixed
/// source addresses match this VM's Ghost IPv6.
///
/// 1. Non-IPv6 → DROP (VM should be doing SIIT, only IPv6 exits)
/// 2. Ghost prefix source → validate src == MY_GHOST_IPV6 → mismatch = DROP
/// 3. Non-Ghost source (e.g. fe80:: for NDP) → PASS
#[classifier]
pub fn tap_guard(ctx: TcContext) -> i32 {
    match try_tap_guard(&ctx) {
        Ok(action) => action,
        Err(_) => TC_ACT_OK, // fail-open
    }
}

fn try_tap_guard(ctx: &TcContext) -> Result<i32, ()> {
    // 1. Non-IPv6 → DROP
    let etype: u16 = ctx.load(12).map_err(|_| ())?;
    if etype != ETH_P_IPV6.to_be() {
        return Ok(TC_ACT_SHOT);
    }

    // 2. Check if source has Ghost prefix
    if !is_ghost_prefix(ctx, IPV6_SRC_ADDR_OFF)? {
        // Non-Ghost source (e.g. fe80:: link-local for NDP) → pass
        return Ok(TC_ACT_OK);
    }

    // 3. Ghost source → validate it matches this VM's Ghost IPv6
    let my_ghost = unsafe { core::ptr::read_volatile(&MY_GHOST_IPV6) };
    let expected = addr_words(&my_ghost);

    let s0: u32 = ctx.load(IPV6_SRC_ADDR_OFF).map_err(|_| ())?;
    let s1: u32 = ctx.load(IPV6_SRC_ADDR_OFF + 4).map_err(|_| ())?;
    let s2: u32 = ctx.load(IPV6_SRC_ADDR_OFF + 8).map_err(|_| ())?;
    let s3: u32 = ctx.load(IPV6_SRC_ADDR_OFF + 12).map_err(|_| ())?;

    if [s0, s1, s2, s3] != expected {
        return Ok(TC_ACT_SHOT); // spoofed Ghost source
    }

    Ok(TC_ACT_OK)
}

// ─── NAT64 Translation Programs ────────────────────────────────

/// LRU hash for NAT64 connection tracking (pinned, shared).
#[map]
static NAT64_CONNTRACK: LruHashMap<Nat64Key, Nat64Value> = LruHashMap::pinned(65536, 0);

/// NAT64 configuration (index 0): node IPv4, physical/bridge interface indices (pinned, shared).
#[map]
static NAT64_CONFIG: Array<Nat64Config> = Array::pinned(1, 0);

const IPV4_HDR_LEN: usize = 20;
const IPV6_HDR_LEN: usize = 40;
const IPPROTO_TCP: u8 = 6;
const IPPROTO_UDP: u8 = 17;
const TCP_CSUM_OFF: usize = 16;
const UDP_CSUM_OFF: usize = 6;
const BPF_F_PSEUDO_HDR: u64 = 0x10;

/// Compute IPv4 header checksum. All inputs in host byte order.
#[inline(always)]
fn ipv4_hdr_csum(total_len: u16, ttl: u8, proto: u8, src: u32, dst: u32) -> u16 {
    let mut s: u32 = 0x4500;
    s += total_len as u32;
    s += 0x4000; // DF
    s += ((ttl as u32) << 8) | (proto as u32);
    s += (src >> 16) & 0xFFFF;
    s += src & 0xFFFF;
    s += (dst >> 16) & 0xFFFF;
    s += dst & 0xFFFF;
    s = (s & 0xFFFF) + (s >> 16);
    s = (s & 0xFFFF) + (s >> 16);
    !(s as u16)
}

/// Build `64:ff9b::ipv4` from a host-order IPv4 address.
#[inline(always)]
fn nat64_addr(ipv4: u32) -> [u8; 16] {
    let b = ipv4.to_be_bytes();
    [0x00, 0x64, 0xff, 0x9b, 0, 0, 0, 0, 0, 0, 0, 0, b[0], b[1], b[2], b[3]]
}

/// Convert 16-byte address to 4 raw u32 words for packet store / csum operations.
/// Each word's in-memory bytes match the IPv6 address bytes on the wire.
#[inline(always)]
fn addr_words(a: &[u8; 16]) -> [u32; 4] {
    [
        u32::from_ne_bytes([a[0], a[1], a[2], a[3]]),
        u32::from_ne_bytes([a[4], a[5], a[6], a[7]]),
        u32::from_ne_bytes([a[8], a[9], a[10], a[11]]),
        u32::from_ne_bytes([a[12], a[13], a[14], a[15]]),
    ]
}

/// Adjust L4 checksum: IPv6 pseudo-header → IPv4 pseudo-header.
/// `s6`/`d6` are raw u32 words (from ctx.load), `s4`/`d4` are raw u32 (to_be of host-order).
#[inline(always)]
fn csum_6to4(
    ctx: &TcContext,
    off: usize,
    s6: [u32; 4],
    d6: [u32; 4],
    s4: u32,
    d4: u32,
) -> Result<(), ()> {
    let f = BPF_F_PSEUDO_HDR | 4;
    ctx.l4_csum_replace(off, s6[0] as u64, s4 as u64, f).map_err(|_| ())?;
    ctx.l4_csum_replace(off, s6[1] as u64, 0, f).map_err(|_| ())?;
    ctx.l4_csum_replace(off, s6[2] as u64, 0, f).map_err(|_| ())?;
    ctx.l4_csum_replace(off, s6[3] as u64, 0, f).map_err(|_| ())?;
    ctx.l4_csum_replace(off, d6[0] as u64, d4 as u64, f).map_err(|_| ())?;
    ctx.l4_csum_replace(off, d6[1] as u64, 0, f).map_err(|_| ())?;
    ctx.l4_csum_replace(off, d6[2] as u64, 0, f).map_err(|_| ())?;
    ctx.l4_csum_replace(off, d6[3] as u64, 0, f).map_err(|_| ())?;
    Ok(())
}

/// Adjust L4 checksum: IPv4 pseudo-header → IPv6 pseudo-header (reverse).
#[inline(always)]
fn csum_4to6(
    ctx: &TcContext,
    off: usize,
    s4: u32,
    d4: u32,
    s6: [u32; 4],
    d6: [u32; 4],
) -> Result<(), ()> {
    let f = BPF_F_PSEUDO_HDR | 4;
    ctx.l4_csum_replace(off, s4 as u64, s6[0] as u64, f).map_err(|_| ())?;
    ctx.l4_csum_replace(off, 0, s6[1] as u64, f).map_err(|_| ())?;
    ctx.l4_csum_replace(off, 0, s6[2] as u64, f).map_err(|_| ())?;
    ctx.l4_csum_replace(off, 0, s6[3] as u64, f).map_err(|_| ())?;
    ctx.l4_csum_replace(off, d4 as u64, d6[0] as u64, f).map_err(|_| ())?;
    ctx.l4_csum_replace(off, 0, d6[1] as u64, f).map_err(|_| ())?;
    ctx.l4_csum_replace(off, 0, d6[2] as u64, f).map_err(|_| ())?;
    ctx.l4_csum_replace(off, 0, d6[3] as u64, f).map_err(|_| ())?;
    Ok(())
}

// ─── NAT64 Egress: IPv6 64:ff9b:: → IPv4 (on k3rs0 bridge) ──

/// TC classifier on k3rs0 bridge egress: translates outbound IPv6 NAT64 traffic
/// (dst in `64:ff9b::/96`) to IPv4 with SNAT to node IPv4, then redirects to
/// the physical interface.
#[classifier]
pub fn nat64_egress(mut ctx: TcContext) -> i32 {
    match try_nat64_egress(&mut ctx) {
        Ok(a) => a,
        Err(_) => TC_ACT_OK, // fail-open
    }
}

fn try_nat64_egress(ctx: &mut TcContext) -> Result<i32, ()> {
    // 1. Check IPv6
    let etype: u16 = ctx.load(12).map_err(|_| ())?;
    if etype != ETH_P_IPV6.to_be() {
        return Ok(TC_ACT_OK);
    }

    // 2. Check dst starts with 64:ff9b::/96
    let d0: u32 = ctx.load(IPV6_DST_ADDR_OFF).map_err(|_| ())?;
    if d0 != NAT64_PREFIX_U32.to_be() {
        return Ok(TC_ACT_OK);
    }
    let d1: u32 = ctx.load(IPV6_DST_ADDR_OFF + 4).map_err(|_| ())?;
    let d2: u32 = ctx.load(IPV6_DST_ADDR_OFF + 8).map_err(|_| ())?;
    if d1 != 0 || d2 != 0 {
        return Ok(TC_ACT_OK);
    }
    let d3: u32 = ctx.load(IPV6_DST_ADDR_OFF + 12).map_err(|_| ())?; // embedded IPv4 (raw)
    let dst_ipv4 = u32::from_be(d3); // host order

    // 3. Read IPv6 header fields
    let payload_len = u16::from_be(ctx.load::<u16>(EthHdr::LEN + 4).map_err(|_| ())?);
    let next_hdr: u8 = ctx.load(EthHdr::LEN + 6).map_err(|_| ())?;
    let hop_limit: u8 = ctx.load(EthHdr::LEN + 7).map_err(|_| ())?;

    // Only TCP/UDP
    if next_hdr != IPPROTO_TCP && next_hdr != IPPROTO_UDP {
        return Ok(TC_ACT_OK);
    }
    if payload_len == 0 {
        return Ok(TC_ACT_OK); // jumbogram — not supported
    }

    // 4. Read src IPv6 (raw u32 words for csum, reconstruct bytes for conntrack)
    let s0: u32 = ctx.load(IPV6_SRC_ADDR_OFF).map_err(|_| ())?;
    let s1: u32 = ctx.load(IPV6_SRC_ADDR_OFF + 4).map_err(|_| ())?;
    let s2: u32 = ctx.load(IPV6_SRC_ADDR_OFF + 8).map_err(|_| ())?;
    let s3: u32 = ctx.load(IPV6_SRC_ADDR_OFF + 12).map_err(|_| ())?;
    let src_v6 = [s0, s1, s2, s3];
    let dst_v6 = [d0, d1, d2, d3];

    // Reconstruct src IPv6 bytes for conntrack value
    let src_ipv6 = {
        let (w0, w1, w2, w3) = (
            s0.to_ne_bytes(),
            s1.to_ne_bytes(),
            s2.to_ne_bytes(),
            s3.to_ne_bytes(),
        );
        [
            w0[0], w0[1], w0[2], w0[3], w1[0], w1[1], w1[2], w1[3],
            w2[0], w2[1], w2[2], w2[3], w3[0], w3[1], w3[2], w3[3],
        ]
    };

    // 5. Read L4 ports (before change_proto shifts offsets)
    let l4 = EthHdr::LEN + IPV6_HDR_LEN;
    let src_port = u16::from_be(ctx.load::<u16>(l4).map_err(|_| ())?);
    let dst_port = u16::from_be(ctx.load::<u16>(l4 + 2).map_err(|_| ())?);

    // 6. Get NAT64 config
    let cfg = NAT64_CONFIG.get(0).ok_or(())?;
    let node_ipv4 = cfg.node_ipv4;
    let phys_ifindex = cfg.phys_ifindex;

    // 7. Store conntrack entry for return-path matching
    let ct_key = Nat64Key {
        protocol: next_hdr,
        _pad: 0,
        src_port,
        dst_ipv4,
        dst_port,
        _pad2: 0,
    };
    let ct_val = Nat64Value { src_ipv6 };
    NAT64_CONNTRACK.insert(&ct_key, &ct_val, 0).map_err(|_| ())?;

    // 8. Change protocol: IPv6 → IPv4 (shrinks L3 header by 20 bytes)
    ctx.change_proto(ETH_P_IP, 0).map_err(|_| ())?;

    // 9. Write IPv4 header (20 bytes at EthHdr::LEN)
    let l3 = EthHdr::LEN;
    let total_len = payload_len + IPV4_HDR_LEN as u16;
    ctx.store(l3, &0x45u8, 0).map_err(|_| ())?; // version=4, IHL=5
    ctx.store(l3 + 1, &0u8, 0).map_err(|_| ())?; // DSCP/ECN
    ctx.store(l3 + 2, &total_len.to_be(), 0).map_err(|_| ())?; // total length
    ctx.store(l3 + 4, &0u16, 0).map_err(|_| ())?; // identification
    ctx.store(l3 + 6, &0x4000u16.to_be(), 0).map_err(|_| ())?; // flags=DF, frag_off=0
    ctx.store(l3 + 8, &hop_limit, 0).map_err(|_| ())?; // TTL
    ctx.store(l3 + 9, &next_hdr, 0).map_err(|_| ())?; // protocol
    ctx.store(l3 + 10, &0u16, 0).map_err(|_| ())?; // checksum placeholder
    let node_raw = node_ipv4.to_be();
    ctx.store(l3 + 12, &node_raw, 0).map_err(|_| ())?; // src = node IPv4 (SNAT)
    ctx.store(l3 + 16, &d3, 0).map_err(|_| ())?; // dst = embedded IPv4

    // 10. Compute and store IPv4 header checksum
    let csum = ipv4_hdr_csum(total_len, hop_limit, next_hdr, node_ipv4, dst_ipv4);
    ctx.store(l3 + 10, &csum.to_be(), 0).map_err(|_| ())?;

    // 11. Adjust L4 checksum for pseudo-header change (IPv6 addrs → IPv4 addrs)
    let csum_off = EthHdr::LEN + IPV4_HDR_LEN
        + if next_hdr == IPPROTO_TCP { TCP_CSUM_OFF } else { UDP_CSUM_OFF };
    csum_6to4(ctx, csum_off, src_v6, dst_v6, node_raw, d3)?;

    // 12. Redirect to physical interface
    Ok(unsafe { bpf_redirect(phys_ifindex, 0) } as i32)
}

// ─── NAT64 Ingress: IPv4 → IPv6 (on physical interface) ──────

/// TC classifier on physical interface ingress: translates return IPv4 traffic
/// matching a NAT64 conntrack entry back to IPv6, restoring the pod's Ghost IPv6
/// destination, then redirects to the k3rs0 bridge ingress.
#[classifier]
pub fn nat64_ingress(mut ctx: TcContext) -> i32 {
    match try_nat64_ingress(&mut ctx) {
        Ok(a) => a,
        Err(_) => TC_ACT_OK, // fail-open
    }
}

fn try_nat64_ingress(ctx: &mut TcContext) -> Result<i32, ()> {
    // 1. Check IPv4
    let etype: u16 = ctx.load(12).map_err(|_| ())?;
    if etype != ETH_P_IP.to_be() {
        return Ok(TC_ACT_OK);
    }

    // Only handle standard IPv4 headers (IHL=5, no options)
    let ver_ihl: u8 = ctx.load(EthHdr::LEN).map_err(|_| ())?;
    if ver_ihl != 0x45 {
        return Ok(TC_ACT_OK);
    }

    let protocol: u8 = ctx.load(EthHdr::LEN + 9).map_err(|_| ())?;
    if protocol != IPPROTO_TCP && protocol != IPPROTO_UDP {
        return Ok(TC_ACT_OK);
    }

    // 2. Read IPv4 addresses
    let src_raw: u32 = ctx.load(EthHdr::LEN + 12).map_err(|_| ())?;
    let dst_raw: u32 = ctx.load(EthHdr::LEN + 16).map_err(|_| ())?;
    let src_ip = u32::from_be(src_raw);
    let dst_ip = u32::from_be(dst_raw);

    // 3. Quick check: dst must be node IPv4 (SNAT destination)
    let cfg = NAT64_CONFIG.get(0).ok_or(())?;
    if dst_ip != cfg.node_ipv4 {
        return Ok(TC_ACT_OK);
    }
    let bridge_ifindex = cfg.bridge_ifindex;

    // 4. Read L4 ports
    let l4 = EthHdr::LEN + IPV4_HDR_LEN;
    let sport = u16::from_be(ctx.load::<u16>(l4).map_err(|_| ())?);
    let dport = u16::from_be(ctx.load::<u16>(l4 + 2).map_err(|_| ())?);

    // 5. Conntrack lookup (reversed: return packet has swapped src/dst ports)
    //    Egress stored: (proto, pod_src_port, remote_ipv4, remote_port)
    //    Return pkt:     src=remote_ipv4, src_port=remote_port, dst_port=pod_src_port
    let ct_key = Nat64Key {
        protocol,
        _pad: 0,
        src_port: dport,  // pod's original src_port
        dst_ipv4: src_ip, // remote server IPv4
        dst_port: sport,  // remote server port
        _pad2: 0,
    };
    let ct = unsafe { NAT64_CONNTRACK.get(&ct_key) }.ok_or(())?;
    let pod_ipv6 = ct.src_ipv6; // copy before packet mutation

    // 6. Read remaining IPv4 fields
    let tot_len = u16::from_be(ctx.load::<u16>(EthHdr::LEN + 2).map_err(|_| ())?);
    let payload_len = tot_len - IPV4_HDR_LEN as u16;
    let ttl: u8 = ctx.load(EthHdr::LEN + 8).map_err(|_| ())?;

    // 7. Construct new IPv6 addresses
    //    src = 64:ff9b:: + remote IPv4 (return sender)
    //    dst = pod's Ghost IPv6 (from conntrack)
    let new_src = nat64_addr(src_ip);
    let new_dst = pod_ipv6;
    let s6 = addr_words(&new_src);
    let d6 = addr_words(&new_dst);

    // 8. Change protocol: IPv4 → IPv6 (grows L3 header by 20 bytes)
    ctx.change_proto(ETH_P_IPV6, 0).map_err(|_| ())?;

    // 9. Write IPv6 header (40 bytes at EthHdr::LEN)
    let l3 = EthHdr::LEN;
    ctx.store(l3, &0x60000000u32.to_be(), 0).map_err(|_| ())?; // ver=6, TC=0, flow=0
    ctx.store(l3 + 4, &payload_len.to_be(), 0).map_err(|_| ())?; // payload length
    ctx.store(l3 + 6, &protocol, 0).map_err(|_| ())?; // next header
    ctx.store(l3 + 7, &ttl, 0).map_err(|_| ())?; // hop limit
    // src IPv6: 64:ff9b:: + remote_ipv4
    ctx.store(l3 + 8, &s6[0], 0).map_err(|_| ())?;
    ctx.store(l3 + 12, &s6[1], 0).map_err(|_| ())?;
    ctx.store(l3 + 16, &s6[2], 0).map_err(|_| ())?;
    ctx.store(l3 + 20, &s6[3], 0).map_err(|_| ())?;
    // dst IPv6: pod's Ghost IPv6
    ctx.store(l3 + 24, &d6[0], 0).map_err(|_| ())?;
    ctx.store(l3 + 28, &d6[1], 0).map_err(|_| ())?;
    ctx.store(l3 + 32, &d6[2], 0).map_err(|_| ())?;
    ctx.store(l3 + 36, &d6[3], 0).map_err(|_| ())?;

    // 10. Adjust L4 checksum for pseudo-header change (IPv4 addrs → IPv6 addrs)
    let csum_off = EthHdr::LEN + IPV6_HDR_LEN
        + if protocol == IPPROTO_TCP { TCP_CSUM_OFF } else { UDP_CSUM_OFF };
    csum_4to6(ctx, csum_off, src_raw, dst_raw, s6, d6)?;

    // 11. Redirect to k3rs0 bridge ingress
    Ok(unsafe { bpf_redirect(bridge_ifindex, 1) } as i32) // BPF_F_INGRESS
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe { core::hint::unreachable_unchecked() }
}
