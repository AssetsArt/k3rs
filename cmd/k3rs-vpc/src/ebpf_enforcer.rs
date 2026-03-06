//! EbpfEnforcer — eBPF-based network enforcement backend using TC classifiers.
//!
//! Host-side: tc_egress_v6 / tc_ingress_v6 attached to TAP/netkit host interfaces
//!            for VPC isolation (uses PEERINGS map, extracts VPC ID from Ghost IPv6 header).
//!
//! Pod/VM-side: siit_in / siit_out attached inside pod netns on eth0 for IPv4↔IPv6 translation.
//!              tap_guard attached on host-side TAP ingress for VM anti-spoofing.
//!              All pod/VM programs use .rodata globals only — no BPF map lookups.

use std::collections::{HashMap, HashSet};
use std::net::Ipv4Addr;
use std::os::unix::io::AsRawFd;
use std::path::Path;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use aya::Ebpf;
use aya::EbpfLoader;
use aya::maps::Array as BpfArray;
use aya::maps::hash_map::HashMap as BpfHashMap;
use aya::programs::tc::{self, SchedClassifier, TcAttachType};
use tracing::{debug, info, warn};

use k3rs_vpc::enforcer::NetworkEnforcer;
use k3rs_vpc_common::{
    EndpointKey, EndpointValue, Nat64Config, PeeringKey, PeeringValue, VpcCidrKey, VpcCidrValue,
};
use pkg_types::vpc::{PeeringDirection, PeeringStatus, Vpc, VpcPeering};

// Safety: these #[repr(C)] types are Copy + 'static with no padding issues.
unsafe impl aya::Pod for VpcCidrKey {}
unsafe impl aya::Pod for VpcCidrValue {}
unsafe impl aya::Pod for PeeringKey {}
unsafe impl aya::Pod for PeeringValue {}
unsafe impl aya::Pod for EndpointKey {}
unsafe impl aya::Pod for EndpointValue {}
unsafe impl aya::Pod for Nat64Config {}

const BPFFS_PIN_DIR: &str = pkg_constants::vm::BPFFS_PIN_DIR;
const EBPF_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/k3rs-vpc-ebpf"));

pub struct EbpfEnforcer {
    bpf: Ebpf,
    /// Platform prefix for Ghost IPv6 construction (e.g. 0xfd6b_3372).
    platform_prefix: u32,
    /// Cluster ID for Ghost IPv6 construction.
    cluster_id: u32,
    active_vpcs: HashSet<u16>,
    /// Reverse lookup: peering_name → list of PeeringKey entries inserted
    peering_to_keys: HashMap<String, Vec<PeeringKey>>,
    /// Per-netkit Ebpf instances: nk_name → Ebpf (holds SIIT programs with baked-in .rodata)
    pod_bpf: HashMap<String, Ebpf>,
    /// Reverse lookup: nk_name → (guest_ipv4_host_order, vpc_id, vpc_network, vpc_mask, ghost_ipv6_bytes)
    nk_info: HashMap<String, (u32, u16, u32, u32, [u8; 16])>,
    /// Per-TAP Ebpf instances: tap_name → Ebpf (holds tap_guard + IPv6 classifiers)
    tap_bpf: HashMap<String, Ebpf>,
    /// Reverse lookup: tap_name → (guest_ipv4_host_order, vpc_id)
    tap_to_ip: HashMap<String, (u32, u16)>,
}

impl EbpfEnforcer {
    /// Create a new EbpfEnforcer with Ghost IPv6 construction parameters.
    pub fn new(platform_prefix: u32, cluster_id: u32) -> Result<Self> {
        if !Path::new("/sys/fs/bpf").exists() {
            bail!("bpffs not mounted at /sys/fs/bpf");
        }

        std::fs::create_dir_all(BPFFS_PIN_DIR)
            .with_context(|| format!("failed to create {}", BPFFS_PIN_DIR))?;

        // Load the main eBPF instance (shared pinned maps: VPC_CIDRS, PEERINGS, NAT64_*)
        let bpf = EbpfLoader::new()
            .set_global("MY_PLATFORM_PREFIX", &platform_prefix, true)
            .set_global("MY_CLUSTER_ID", &cluster_id, true)
            .map_pin_path(BPFFS_PIN_DIR)
            .load(EBPF_BYTES)
            .context("failed to load eBPF programs")?;

        Ok(Self {
            bpf,
            platform_prefix,
            cluster_id,
            active_vpcs: HashSet::new(),
            peering_to_keys: HashMap::new(),
            pod_bpf: HashMap::new(),
            nk_info: HashMap::new(),
            tap_bpf: HashMap::new(),
            tap_to_ip: HashMap::new(),
        })
    }

    /// Parse a CIDR string like "10.0.1.0/24" into (network_u32, mask_u32) in host byte order.
    fn parse_cidr(cidr: &str) -> Result<(u32, u32)> {
        let (addr_str, prefix_str) = cidr
            .split_once('/')
            .ok_or_else(|| anyhow::anyhow!("invalid CIDR: {}", cidr))?;
        let addr: Ipv4Addr = addr_str.parse().context("invalid CIDR network address")?;
        let prefix_len: u32 = prefix_str.parse().context("invalid CIDR prefix length")?;
        if prefix_len > 32 {
            bail!("invalid CIDR prefix length: {}", prefix_len);
        }
        let network = u32::from(addr);
        let mask = if prefix_len == 0 {
            0
        } else {
            !0u32 << (32 - prefix_len)
        };
        Ok((network, mask))
    }

    /// Read interface index from sysfs.
    fn ifindex(iface: &str) -> Result<u32> {
        let path = format!("/sys/class/net/{}/ifindex", iface);
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read ifindex for {}", iface))?;
        content
            .trim()
            .parse::<u32>()
            .with_context(|| format!("invalid ifindex for {}", iface))
    }
}

#[async_trait]
impl NetworkEnforcer for EbpfEnforcer {
    fn name(&self) -> &str {
        "ebpf"
    }

    async fn init(&mut self) -> Result<()> {
        std::fs::create_dir_all(BPFFS_PIN_DIR)
            .with_context(|| format!("failed to create {}", BPFFS_PIN_DIR))?;

        // Detect crash recovery: if pinned maps already exist, aya reuses them
        // automatically via map_pin_path(). Pre-populate in-memory state from
        // the pinned VPC_CIDRS and PEERINGS maps so rebuild() can diff correctly.
        let has_pinned_maps = Path::new(BPFFS_PIN_DIR).join("VPC_CIDRS").exists();
        if has_pinned_maps {
            info!(
                "ebpf: detected existing pinned maps at {} — recovering state from previous run",
                BPFFS_PIN_DIR
            );

            // Recover active_vpcs from pinned VPC_CIDRS map
            if let Ok(map) = BpfHashMap::<&aya::maps::MapData, VpcCidrKey, VpcCidrValue>::try_from(
                self.bpf.map("VPC_CIDRS").unwrap(),
            ) {
                for item in map.iter() {
                    if let Ok((k, _)) = item {
                        self.active_vpcs.insert(k.vpc_id);
                    }
                }
                info!(
                    "ebpf: recovered {} VPC entries from pinned maps",
                    self.active_vpcs.len()
                );
            }

            // Recover peering_to_keys from pinned PEERINGS map (stored as synthetic keys)
            if let Ok(map) = BpfHashMap::<&aya::maps::MapData, PeeringKey, PeeringValue>::try_from(
                self.bpf.map("PEERINGS").unwrap(),
            ) {
                let mut count = 0u32;
                for item in map.iter() {
                    if let Ok((k, _)) = item {
                        // We don't know the peering name from the map alone; rebuild() will
                        // re-populate peering_to_keys with proper names from VpcStore data.
                        // Just count for logging here.
                        count += 1;
                        let _ = k;
                    }
                }
                info!("ebpf: found {} peering entries in pinned maps", count);
            }

            // Recover ENDPOINTS count from pinned map
            if let Ok(map) =
                BpfHashMap::<&aya::maps::MapData, EndpointKey, EndpointValue>::try_from(
                    self.bpf.map("ENDPOINTS").unwrap(),
                )
            {
                let count = map.iter().filter(|i| i.is_ok()).count();
                if count > 0 {
                    info!("ebpf: found {} endpoint entries in pinned maps", count);
                }
            }
        } else {
            info!("ebpf: fresh start, pin dir={}", BPFFS_PIN_DIR);
        }

        Ok(())
    }

    async fn ensure_vpc(&mut self, vpc_id: u16, cidr: &str) -> Result<()> {
        if self.active_vpcs.contains(&vpc_id) {
            return Ok(());
        }

        let (network, mask) = Self::parse_cidr(cidr)?;

        let key = VpcCidrKey { vpc_id, _pad: 0 };
        let value = VpcCidrValue { network, mask };

        let mut map: BpfHashMap<&mut aya::maps::MapData, VpcCidrKey, VpcCidrValue> =
            BpfHashMap::try_from(
                self.bpf
                    .map_mut("VPC_CIDRS")
                    .context("VPC_CIDRS map not found")?,
            )?;
        map.insert(key, value, 0)?;

        self.active_vpcs.insert(vpc_id);
        info!("ebpf: ensured VPC vpc_id={} cidr={}", vpc_id, cidr);
        Ok(())
    }

    async fn remove_vpc(&mut self, vpc_id: u16) -> Result<()> {
        if !self.active_vpcs.remove(&vpc_id) {
            return Ok(());
        }

        let key = VpcCidrKey { vpc_id, _pad: 0 };

        let mut map: BpfHashMap<&mut aya::maps::MapData, VpcCidrKey, VpcCidrValue> =
            BpfHashMap::try_from(
                self.bpf
                    .map_mut("VPC_CIDRS")
                    .context("VPC_CIDRS map not found")?,
            )?;
        map.remove(&key).ok();

        info!("ebpf: removed VPC vpc_id={}", vpc_id);
        Ok(())
    }

    async fn install_tap_rules(
        &mut self,
        tap_name: &str,
        guest_ipv4: &str,
        ghost_ipv6: &str,
        vpc_id: u16,
        vpc_cidr: &str,
    ) -> Result<()> {
        let addr: Ipv4Addr = guest_ipv4.parse().context("invalid tap IPv4")?;
        let ip_host = u32::from(addr);

        let ipv6_addr: std::net::Ipv6Addr = ghost_ipv6.parse().context("invalid tap Ghost IPv6")?;
        let ipv6_bytes: [u8; 16] = ipv6_addr.octets();

        let (vpc_network, vpc_mask) = Self::parse_cidr(vpc_cidr)?;

        // Load per-TAP Ebpf with .rodata globals baked in (translation + anti-spoof)
        let mut tap_ebpf = EbpfLoader::new()
            .set_global("MY_GHOST_IPV6", &ipv6_bytes, true)
            .set_global("MY_GUEST_IPV4", &ip_host, true)
            .set_global("MY_VPC_ID", &vpc_id, true)
            .set_global("MY_VPC_NETWORK", &vpc_network, true)
            .set_global("MY_VPC_MASK", &vpc_mask, true)
            .set_global("MY_PLATFORM_PREFIX", &self.platform_prefix, true)
            .set_global("MY_CLUSTER_ID", &self.cluster_id, true)
            .map_pin_path(BPFFS_PIN_DIR)
            .load(EBPF_BYTES)
            .with_context(|| format!("failed to load per-TAP eBPF for {}", tap_name))?;

        // Add clsact qdisc
        if let Err(e) = tc::qdisc_add_clsact(tap_name) {
            let msg = format!("{}", e);
            if !msg.contains("exist") {
                warn!("ebpf: failed to add clsact qdisc to {}: {}", tap_name, e);
            }
        }

        // Attach tap_guard on ingress (VM → host): anti-spoofing, IPv6-only enforcement
        let guard: &mut SchedClassifier = tap_ebpf
            .program_mut("tap_guard")
            .context("tap_guard program not found in per-TAP instance")?
            .try_into()?;
        guard.load()?;
        guard
            .attach(tap_name, TcAttachType::Ingress)
            .with_context(|| format!("failed to attach tap_guard to {}", tap_name))?;

        // Attach shared IPv6 VPC isolation classifiers on host-side TAP
        let ingress_v6: &mut SchedClassifier = self
            .bpf
            .program_mut("tc_ingress_v6")
            .context("tc_ingress_v6 program not found")?
            .try_into()?;
        ingress_v6.load().ok();
        ingress_v6
            .attach(tap_name, TcAttachType::Ingress)
            .with_context(|| format!("failed to attach tc_ingress_v6 to {}", tap_name))?;

        let egress_v6: &mut SchedClassifier = self
            .bpf
            .program_mut("tc_egress_v6")
            .context("tc_egress_v6 program not found")?
            .try_into()?;
        egress_v6.load().ok();
        egress_v6
            .attach(tap_name, TcAttachType::Egress)
            .with_context(|| format!("failed to attach tc_egress_v6 to {}", tap_name))?;

        self.tap_bpf.insert(tap_name.to_string(), tap_ebpf);
        self.tap_to_ip
            .insert(tap_name.to_string(), (ip_host, vpc_id));

        debug!(
            "ebpf: installed per-TAP rules (guard + IPv6 isolation) for tap={} ipv4={} ipv6={} vpc_id={}",
            tap_name, guest_ipv4, ghost_ipv6, vpc_id
        );
        Ok(())
    }

    async fn remove_tap_rules(&mut self, tap_name: &str) -> Result<()> {
        if self.tap_to_ip.remove(tap_name).is_some() {
            self.tap_bpf.remove(tap_name);
            debug!("ebpf: removed per-TAP rules for tap={}", tap_name);
        }
        Ok(())
    }

    async fn install_netkit_rules(
        &mut self,
        nk_name: &str,
        guest_ipv4: &str,
        ghost_ipv6: &str,
        vpc_id: u16,
        vpc_cidr: &str,
        container_pid: u32,
    ) -> Result<()> {
        let ipv4_addr: Ipv4Addr = guest_ipv4.parse().context("invalid netkit IPv4")?;
        let ip_host = u32::from(ipv4_addr);

        let ipv6_addr: std::net::Ipv6Addr =
            ghost_ipv6.parse().context("invalid netkit Ghost IPv6")?;
        let ipv6_bytes: [u8; 16] = ipv6_addr.octets();

        let (vpc_network, vpc_mask) = Self::parse_cidr(vpc_cidr)?;

        // Load per-pod Ebpf with .rodata globals baked in
        let mut pod_ebpf = EbpfLoader::new()
            .set_global("MY_GHOST_IPV6", &ipv6_bytes, true)
            .set_global("MY_GUEST_IPV4", &ip_host, true)
            .set_global("MY_VPC_ID", &vpc_id, true)
            .set_global("MY_VPC_NETWORK", &vpc_network, true)
            .set_global("MY_VPC_MASK", &vpc_mask, true)
            .set_global("MY_PLATFORM_PREFIX", &self.platform_prefix, true)
            .set_global("MY_CLUSTER_ID", &self.cluster_id, true)
            .map_pin_path(BPFFS_PIN_DIR)
            .load(EBPF_BYTES)
            .with_context(|| format!("failed to load per-pod eBPF for {}", nk_name))?;

        let nk_name_owned = nk_name.to_string();
        let pid = container_pid;

        // Load SIIT programs before entering the netns
        let siit_in: &mut SchedClassifier = pod_ebpf
            .program_mut("siit_in")
            .context("siit_in program not found in per-pod instance")?
            .try_into()?;
        siit_in.load()?;

        let siit_out: &mut SchedClassifier = pod_ebpf
            .program_mut("siit_out")
            .context("siit_out program not found in per-pod instance")?
            .try_into()?;
        siit_out.load()?;

        // Enter pod netns, attach SIIT translators to eth0
        tokio::task::spawn_blocking(move || -> Result<()> {
            let pod_ns_path = format!("/proc/{}/ns/net", pid);
            let pod_ns = std::fs::File::open(&pod_ns_path)
                .with_context(|| format!("failed to open pod netns at {}", pod_ns_path))?;
            let orig_ns =
                std::fs::File::open("/proc/self/ns/net").context("failed to open current netns")?;

            let ret = unsafe { libc::setns(pod_ns.as_raw_fd(), libc::CLONE_NEWNET) };
            if ret != 0 {
                bail!(
                    "setns into pod netns failed: {}",
                    std::io::Error::last_os_error()
                );
            }

            let attach_result = (|| -> Result<()> {
                if let Err(e) = tc::qdisc_add_clsact("eth0") {
                    let msg = format!("{}", e);
                    if !msg.contains("exist") {
                        warn!("ebpf: failed to add clsact qdisc to eth0 in pod: {}", e);
                    }
                }

                siit_in
                    .attach("eth0", TcAttachType::Egress)
                    .with_context(|| {
                        format!(
                            "failed to attach siit_in to eth0 egress in pod (nk={})",
                            nk_name_owned
                        )
                    })?;

                siit_out
                    .attach("eth0", TcAttachType::Ingress)
                    .with_context(|| {
                        format!(
                            "failed to attach siit_out to eth0 ingress in pod (nk={})",
                            nk_name_owned
                        )
                    })?;

                Ok(())
            })();

            let restore_ret = unsafe { libc::setns(orig_ns.as_raw_fd(), libc::CLONE_NEWNET) };
            if restore_ret != 0 {
                bail!(
                    "setns restore to original netns failed: {}",
                    std::io::Error::last_os_error()
                );
            }

            attach_result
        })
        .await
        .context("spawn_blocking panicked")?
        .with_context(|| {
            format!(
                "failed to attach SIIT programs inside pod netns (pid={}, nk={})",
                container_pid, nk_name
            )
        })?;

        // Add clsact qdisc to host-side netkit (for IPv6 isolation classifiers)
        if let Err(e) = tc::qdisc_add_clsact(nk_name) {
            let msg = format!("{}", e);
            if !msg.contains("exist") {
                warn!("ebpf: failed to add clsact qdisc to {}: {}", nk_name, e);
            }
        }

        // Attach shared IPv6 VPC isolation classifiers on host-side netkit
        let ingress_v6: &mut SchedClassifier = self
            .bpf
            .program_mut("tc_ingress_v6")
            .context("tc_ingress_v6 program not found")?
            .try_into()?;
        ingress_v6.load().ok();
        ingress_v6
            .attach(nk_name, TcAttachType::Ingress)
            .with_context(|| format!("failed to attach tc_ingress_v6 to {}", nk_name))?;

        let egress_v6: &mut SchedClassifier = self
            .bpf
            .program_mut("tc_egress_v6")
            .context("tc_egress_v6 program not found")?
            .try_into()?;
        egress_v6.load().ok();
        egress_v6
            .attach(nk_name, TcAttachType::Egress)
            .with_context(|| format!("failed to attach tc_egress_v6 to {}", nk_name))?;

        // Insert ENDPOINTS entry: ghost_ipv6 → nk_ifindex (for bpf_redirect_peer)
        let nk_ifindex = Self::ifindex(nk_name)?;
        let ep_key = EndpointKey {
            ghost_ipv6: ipv6_bytes,
        };
        let ep_value = EndpointValue {
            nk_ifindex,
            _pad: 0,
        };
        let mut ep_map: BpfHashMap<&mut aya::maps::MapData, EndpointKey, EndpointValue> =
            BpfHashMap::try_from(
                self.bpf
                    .map_mut("ENDPOINTS")
                    .context("ENDPOINTS map not found")?,
            )?;
        ep_map.insert(ep_key, ep_value, 0)?;

        self.pod_bpf.insert(nk_name.to_string(), pod_ebpf);
        self.nk_info.insert(
            nk_name.to_string(),
            (ip_host, vpc_id, vpc_network, vpc_mask, ipv6_bytes),
        );

        debug!(
            "ebpf: installed SIIT (pod netns pid={}) + IPv6 isolation + ENDPOINT on nk={} ipv4={} ipv6={} vpc_id={}",
            container_pid, nk_name, guest_ipv4, ghost_ipv6, vpc_id
        );
        Ok(())
    }

    async fn remove_netkit_rules(&mut self, nk_name: &str) -> Result<()> {
        if let Some((_, _, _, _, ipv6_bytes)) = self.nk_info.remove(nk_name) {
            // Remove ENDPOINTS entry
            let ep_key = EndpointKey {
                ghost_ipv6: ipv6_bytes,
            };
            if let Ok(mut ep_map) = BpfHashMap::<&mut aya::maps::MapData, EndpointKey, EndpointValue>::try_from(
                self.bpf.map_mut("ENDPOINTS").unwrap(),
            ) {
                ep_map.remove(&ep_key).ok();
            }

            self.pod_bpf.remove(nk_name);
            debug!("ebpf: removed netkit rules + ENDPOINT for nk={}", nk_name);
        }
        Ok(())
    }

    async fn install_peering_rules(&mut self, peering: &VpcPeering, vpcs: &[Vpc]) -> Result<()> {
        if peering.status != PeeringStatus::Active {
            return Ok(());
        }

        let vpc_a = vpcs.iter().find(|v| v.name == peering.vpc_a);
        let vpc_b = vpcs.iter().find(|v| v.name == peering.vpc_b);

        let (vpc_a, vpc_b) = match (vpc_a, vpc_b) {
            (Some(a), Some(b)) => (a, b),
            _ => {
                warn!(
                    "ebpf: peering '{}' references unknown VPC(s) ({}, {})",
                    peering.name, peering.vpc_a, peering.vpc_b
                );
                return Ok(());
            }
        };

        let allowed = PeeringValue { allowed: 1 };
        let mut keys = Vec::new();

        let mut map: BpfHashMap<&mut aya::maps::MapData, PeeringKey, PeeringValue> =
            BpfHashMap::try_from(
                self.bpf
                    .map_mut("PEERINGS")
                    .context("PEERINGS map not found")?,
            )?;

        match peering.direction {
            PeeringDirection::Bidirectional => {
                let k1 = PeeringKey {
                    src_vpc_id: vpc_a.vpc_id,
                    dst_vpc_id: vpc_b.vpc_id,
                };
                map.insert(k1, allowed, 0)?;
                keys.push(k1);

                let k2 = PeeringKey {
                    src_vpc_id: vpc_b.vpc_id,
                    dst_vpc_id: vpc_a.vpc_id,
                };
                map.insert(k2, allowed, 0)?;
                keys.push(k2);
            }
            PeeringDirection::InitiatorOnly => {
                let k1 = PeeringKey {
                    src_vpc_id: vpc_a.vpc_id,
                    dst_vpc_id: vpc_b.vpc_id,
                };
                map.insert(k1, allowed, 0)?;
                keys.push(k1);
            }
        }

        self.peering_to_keys.insert(peering.name.clone(), keys);

        info!(
            "ebpf: installed peering rules for '{}' ({} <-> {} {:?})",
            peering.name, peering.vpc_a, peering.vpc_b, peering.direction
        );
        Ok(())
    }

    async fn remove_peering_rules(&mut self, peering_name: &str) -> Result<()> {
        if let Some(keys) = self.peering_to_keys.remove(peering_name) {
            let mut map: BpfHashMap<&mut aya::maps::MapData, PeeringKey, PeeringValue> =
                BpfHashMap::try_from(
                    self.bpf
                        .map_mut("PEERINGS")
                        .context("PEERINGS map not found")?,
                )?;

            for key in &keys {
                map.remove(key).ok();
            }

            debug!("ebpf: removed peering rules for '{}'", peering_name);
        }
        Ok(())
    }

    async fn install_nat64(
        &mut self,
        node_ipv4: &str,
        bridge_name: &str,
        phys_name: &str,
    ) -> Result<()> {
        let addr: Ipv4Addr = node_ipv4.parse().context("invalid node IPv4 for NAT64")?;
        let node_ip = u32::from(addr);
        let phys_ifindex = Self::ifindex(phys_name)?;

        let config = Nat64Config {
            node_ipv4: node_ip,
            phys_ifindex,
            _pad: [0; 2],
        };
        let mut map: BpfArray<&mut aya::maps::MapData, Nat64Config> = BpfArray::try_from(
            self.bpf
                .map_mut("NAT64_CONFIG")
                .context("NAT64_CONFIG map not found")?,
        )?;
        map.set(0, config, 0)?;

        for iface in [bridge_name, phys_name] {
            if let Err(e) = tc::qdisc_add_clsact(iface) {
                let msg = format!("{}", e);
                if !msg.contains("exist") {
                    warn!("ebpf: failed to add clsact qdisc to {}: {}", iface, e);
                }
            }
        }

        let egress: &mut SchedClassifier = self
            .bpf
            .program_mut("nat64_egress")
            .context("nat64_egress program not found")?
            .try_into()?;
        egress.load().ok();
        egress
            .attach(bridge_name, TcAttachType::Egress)
            .with_context(|| format!("failed to attach nat64_egress to {}", bridge_name))?;

        let ingress: &mut SchedClassifier = self
            .bpf
            .program_mut("nat64_ingress")
            .context("nat64_ingress program not found")?
            .try_into()?;
        ingress.load().ok();
        ingress
            .attach(phys_name, TcAttachType::Ingress)
            .with_context(|| format!("failed to attach nat64_ingress to {}", phys_name))?;

        info!(
            "ebpf: installed NAT64 (node_ipv4={}, bridge={}, phys={})",
            node_ipv4, bridge_name, phys_name
        );
        Ok(())
    }

    async fn remove_nat64(&mut self) -> Result<()> {
        let config = Nat64Config {
            node_ipv4: 0,
            phys_ifindex: 0,
            _pad: [0; 2],
        };
        if let Ok(map_data) = self.bpf.map_mut("NAT64_CONFIG") {
            if let Ok(mut map) =
                BpfArray::<&mut aya::maps::MapData, Nat64Config>::try_from(map_data)
            {
                map.set(0, config, 0).ok();
            }
        }
        info!("ebpf: removed NAT64 config");
        Ok(())
    }

    async fn snapshot(&self) -> Result<String> {
        let mut out = String::new();
        out.push_str("=== eBPF Enforcer Snapshot ===\n");

        out.push_str(&format!(
            "\nplatform_prefix=0x{:08x} cluster_id={}\n",
            self.platform_prefix, self.cluster_id
        ));

        // VPC CIDRs
        out.push_str("\n[VPC CIDRs]\n");
        let map: BpfHashMap<&aya::maps::MapData, VpcCidrKey, VpcCidrValue> = BpfHashMap::try_from(
            self.bpf
                .map("VPC_CIDRS")
                .context("VPC_CIDRS map not found")?,
        )?;
        for item in map.iter() {
            if let Ok((k, v)) = item {
                let net = Ipv4Addr::from(v.network);
                let prefix = v.mask.leading_ones();
                out.push_str(&format!("  vpc_id={} cidr={}/{}\n", k.vpc_id, net, prefix));
            }
        }

        // Peerings
        out.push_str("\n[Peerings]\n");
        let map: BpfHashMap<&aya::maps::MapData, PeeringKey, PeeringValue> =
            BpfHashMap::try_from(self.bpf.map("PEERINGS").context("PEERINGS map not found")?)?;
        for item in map.iter() {
            if let Ok((k, v)) = item {
                out.push_str(&format!(
                    "  vpc {} → vpc {} (allowed={})\n",
                    k.src_vpc_id, k.dst_vpc_id, v.allowed
                ));
            }
        }

        // Endpoints
        out.push_str("\n[Endpoints (redirect_peer)]\n");
        if let Ok(map) = BpfHashMap::<&aya::maps::MapData, EndpointKey, EndpointValue>::try_from(
            self.bpf.map("ENDPOINTS").unwrap(),
        ) {
            for item in map.iter() {
                if let Ok((k, v)) = item {
                    let ipv6 = std::net::Ipv6Addr::from(k.ghost_ipv6);
                    out.push_str(&format!(
                        "  {} → ifindex={}\n",
                        ipv6, v.nk_ifindex
                    ));
                }
            }
        }

        // Per-netkit SIIT instances
        out.push_str("\n[Per-Netkit SIIT Instances]\n");
        for (nk, (ipv4, vpc_id, _, _, _)) in &self.nk_info {
            let addr = Ipv4Addr::from(*ipv4);
            out.push_str(&format!("  nk={} ipv4={} vpc_id={}\n", nk, addr, vpc_id));
        }

        // Per-TAP instances
        out.push_str("\n[Per-TAP Instances]\n");
        for (tap, (ipv4, vpc_id)) in &self.tap_to_ip {
            let addr = Ipv4Addr::from(*ipv4);
            out.push_str(&format!("  tap={} ipv4={} vpc_id={}\n", tap, addr, vpc_id));
        }

        Ok(out)
    }

    async fn cleanup(&mut self) -> Result<()> {
        if Path::new(BPFFS_PIN_DIR).exists() {
            std::fs::remove_dir_all(BPFFS_PIN_DIR).ok();
        }

        self.active_vpcs.clear();
        self.peering_to_keys.clear();
        self.pod_bpf.clear();
        self.nk_info.clear();
        self.tap_bpf.clear();
        self.tap_to_ip.clear();

        info!("ebpf: cleaned up all state");
        Ok(())
    }
}
