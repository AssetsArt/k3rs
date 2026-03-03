//! EbpfEnforcer — eBPF-based network enforcement backend using TC classifiers.
//!
//! Uses Aya to load TC classifier programs and manage BPF hash maps for
//! VPC membership, CIDR info, and peering relationships.

use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, Ipv6Addr};
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
    Nat64Config, PeeringKey, PeeringValue, PodKey, PodValue, VpcCidrKey, VpcCidrValue, VpcPodKey,
    VpcPodValue,
};
use pkg_types::vpc::{PeeringDirection, PeeringStatus, Vpc, VpcPeering};

// Safety: these #[repr(C)] types are Copy + 'static with no padding issues.
unsafe impl aya::Pod for PodKey {}
unsafe impl aya::Pod for PodValue {}
unsafe impl aya::Pod for VpcCidrKey {}
unsafe impl aya::Pod for VpcCidrValue {}
unsafe impl aya::Pod for PeeringKey {}
unsafe impl aya::Pod for PeeringValue {}
unsafe impl aya::Pod for Nat64Config {}
unsafe impl aya::Pod for VpcPodKey {}
unsafe impl aya::Pod for VpcPodValue {}

const BPFFS_PIN_DIR: &str = "/sys/fs/bpf/k3rs_vpc";
const EBPF_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/k3rs-vpc-ebpf"));

pub struct EbpfEnforcer {
    bpf: Ebpf,
    active_vpcs: HashSet<u16>,
    /// Reverse lookup: pod_id → (ipv4 in host byte order, vpc_id)
    pod_to_ip: HashMap<String, (u32, u16)>,
    /// Reverse lookup: tap_name → (ipv4 in host byte order, vpc_id)
    tap_to_ip: HashMap<String, (u32, u16)>,
    /// Reverse lookup: peering_name → list of PeeringKey entries inserted
    peering_to_keys: HashMap<String, Vec<PeeringKey>>,
    /// Per-netkit Ebpf instances: nk_name → Ebpf (holds SIIT programs with baked-in .rodata)
    pod_bpf: HashMap<String, Ebpf>,
    /// Reverse lookup: nk_name → (guest_ipv4_host_order, vpc_id) for VPC_PODS cleanup
    nk_vpc_pods: HashMap<String, (u32, u16)>,
    /// Per-TAP Ebpf instances: tap_name → Ebpf (holds tc_egress/tc_ingress with baked-in .rodata)
    tap_bpf: HashMap<String, Ebpf>,
}

impl EbpfEnforcer {
    /// Try to create a new EbpfEnforcer. Returns Err if eBPF is not available.
    pub fn new() -> Result<Self> {
        // Check basic eBPF support
        if !Path::new("/sys/fs/bpf").exists() {
            bail!("bpffs not mounted at /sys/fs/bpf");
        }

        // Ensure pin directory exists before loading (pinned maps need it)
        std::fs::create_dir_all(BPFFS_PIN_DIR)
            .with_context(|| format!("failed to create {}", BPFFS_PIN_DIR))?;

        // Load the main eBPF instance with map_pin_path for shared pinned maps
        let bpf = EbpfLoader::new()
            .map_pin_path(BPFFS_PIN_DIR)
            .load(EBPF_BYTES)
            .context("failed to load eBPF programs")?;

        Ok(Self {
            bpf,
            active_vpcs: HashSet::new(),
            pod_to_ip: HashMap::new(),
            tap_to_ip: HashMap::new(),
            peering_to_keys: HashMap::new(),
            pod_bpf: HashMap::new(),
            nk_vpc_pods: HashMap::new(),
            tap_bpf: HashMap::new(),
        })
    }

    /// Parse a CIDR string like "10.0.1.0/24" into (network_u32, mask_u32) in host byte order.
    fn parse_cidr(cidr: &str) -> Result<(u32, u32)> {
        let parts: Vec<&str> = cidr.split('/').collect();
        if parts.len() != 2 {
            bail!("invalid CIDR: {}", cidr);
        }
        let addr: Ipv4Addr = parts[0].parse().context("invalid CIDR network address")?;
        let prefix_len: u32 = parts[1].parse().context("invalid CIDR prefix length")?;
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
        // Ensure bpffs pin directory exists
        std::fs::create_dir_all(BPFFS_PIN_DIR)
            .with_context(|| format!("failed to create {}", BPFFS_PIN_DIR))?;

        info!("ebpf: initialized, pin dir={}", BPFFS_PIN_DIR);
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

    async fn install_pod_rules(
        &mut self,
        pod_id: &str,
        guest_ipv4: &str,
        vpc_id: u16,
    ) -> Result<()> {
        let addr: Ipv4Addr = guest_ipv4.parse().context("invalid pod IPv4")?;
        let ip_host = u32::from(addr);

        let key = PodKey { ipv4_addr: ip_host };
        let value = PodValue { vpc_id, _pad: 0 };

        let mut map: BpfHashMap<&mut aya::maps::MapData, PodKey, PodValue> = BpfHashMap::try_from(
            self.bpf
                .map_mut("VPC_MEMBERSHIP")
                .context("VPC_MEMBERSHIP map not found")?,
        )?;
        map.insert(key, value, 0)?;

        self.pod_to_ip.insert(pod_id.to_string(), (ip_host, vpc_id));

        debug!(
            "ebpf: installed pod rules for pod={} ipv4={} vpc_id={}",
            pod_id, guest_ipv4, vpc_id
        );
        Ok(())
    }

    async fn remove_pod_rules(&mut self, pod_id: &str) -> Result<()> {
        if let Some((ip_host, _vpc_id)) = self.pod_to_ip.remove(pod_id) {
            let key = PodKey { ipv4_addr: ip_host };

            let mut map: BpfHashMap<&mut aya::maps::MapData, PodKey, PodValue> =
                BpfHashMap::try_from(
                    self.bpf
                        .map_mut("VPC_MEMBERSHIP")
                        .context("VPC_MEMBERSHIP map not found")?,
                )?;
            map.remove(&key).ok();

            debug!("ebpf: removed pod rules for pod={}", pod_id);
        }
        Ok(())
    }

    async fn install_tap_rules(
        &mut self,
        tap_name: &str,
        guest_ipv4: &str,
        vpc_id: u16,
    ) -> Result<()> {
        let addr: Ipv4Addr = guest_ipv4.parse().context("invalid tap IPv4")?;
        let ip_host = u32::from(addr);

        // 1. Insert into VPC_PODS (VPC-scoped key for same-VPC checks in per-pod tc_egress/tc_ingress)
        //    TAP pods don't use SIIT, so ghost_ipv6 is a dummy value.
        let vpc_pod_key = VpcPodKey {
            vpc_id,
            _pad: 0,
            ipv4_addr: ip_host,
        };
        let vpc_pod_value = VpcPodValue {
            ghost_ipv6: [0u8; 16],
        };
        {
            let mut map: BpfHashMap<&mut aya::maps::MapData, VpcPodKey, VpcPodValue> =
                BpfHashMap::try_from(
                    self.bpf
                        .map_mut("VPC_PODS")
                        .context("VPC_PODS map not found")?,
                )?;
            map.insert(vpc_pod_key, vpc_pod_value, 0)?;
        }

        // 2. Load per-TAP Ebpf with .rodata globals baked in
        let mut tap_ebpf = EbpfLoader::new()
            .set_global("MY_GUEST_IPV4", &ip_host, true)
            .set_global("MY_VPC_ID", &vpc_id, true)
            .map_pin_path(BPFFS_PIN_DIR)
            .load(EBPF_BYTES)
            .with_context(|| format!("failed to load per-TAP eBPF for {}", tap_name))?;

        // 3. Add clsact qdisc
        if let Err(e) = tc::qdisc_add_clsact(tap_name) {
            let msg = format!("{}", e);
            if !msg.contains("exist") {
                warn!("ebpf: failed to add clsact qdisc to {}: {}", tap_name, e);
            }
        }

        // 4. From per-TAP instance: load + attach tc_egress (egress) and tc_ingress (ingress)
        let egress: &mut SchedClassifier = tap_ebpf
            .program_mut("tc_egress")
            .context("tc_egress program not found in per-TAP instance")?
            .try_into()?;
        egress.load()?;
        egress
            .attach(tap_name, TcAttachType::Egress)
            .with_context(|| format!("failed to attach tc_egress to {}", tap_name))?;

        let ingress: &mut SchedClassifier = tap_ebpf
            .program_mut("tc_ingress")
            .context("tc_ingress program not found in per-TAP instance")?
            .try_into()?;
        ingress.load()?;
        ingress
            .attach(tap_name, TcAttachType::Ingress)
            .with_context(|| format!("failed to attach tc_ingress to {}", tap_name))?;

        // 5. From main instance: attach IPv6 VPC classifiers (already loaded, shared)
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

        // 6. Store per-TAP instance and cleanup info
        self.tap_bpf.insert(tap_name.to_string(), tap_ebpf);
        self.tap_to_ip
            .insert(tap_name.to_string(), (ip_host, vpc_id));

        debug!(
            "ebpf: installed per-TAP rules for tap={} ipv4={} vpc_id={}",
            tap_name, guest_ipv4, vpc_id
        );
        Ok(())
    }

    async fn remove_tap_rules(&mut self, tap_name: &str) -> Result<()> {
        if let Some((ip_host, vpc_id)) = self.tap_to_ip.remove(tap_name) {
            // Remove VPC_PODS entry from shared pinned map
            let vpc_pod_key = VpcPodKey {
                vpc_id,
                _pad: 0,
                ipv4_addr: ip_host,
            };
            {
                let mut map: BpfHashMap<&mut aya::maps::MapData, VpcPodKey, VpcPodValue> =
                    BpfHashMap::try_from(
                        self.bpf
                            .map_mut("VPC_PODS")
                            .context("VPC_PODS map not found")?,
                    )?;
                map.remove(&vpc_pod_key).ok();
            }

            // Drop per-TAP Ebpf instance — kernel cleans up TC programs when TAP is deleted
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
        container_pid: u32,
    ) -> Result<()> {
        let ipv4_addr: Ipv4Addr = guest_ipv4.parse().context("invalid netkit IPv4")?;
        let ip_host = u32::from(ipv4_addr);

        let ipv6_addr: Ipv6Addr = ghost_ipv6.parse().context("invalid netkit Ghost IPv6")?;
        let ipv6_bytes: [u8; 16] = ipv6_addr.octets();

        // 1. Populate VPC_PODS in main instance (shared pinned map)
        let vpc_pod_key = VpcPodKey {
            vpc_id,
            _pad: 0,
            ipv4_addr: ip_host,
        };
        let vpc_pod_value = VpcPodValue {
            ghost_ipv6: ipv6_bytes,
        };
        {
            let mut map: BpfHashMap<&mut aya::maps::MapData, VpcPodKey, VpcPodValue> =
                BpfHashMap::try_from(
                    self.bpf
                        .map_mut("VPC_PODS")
                        .context("VPC_PODS map not found")?,
                )?;
            map.insert(vpc_pod_key, vpc_pod_value, 0)?;
        }

        // 2. Load per-pod Ebpf with .rodata globals baked in
        //    .rodata is PinningType::None — each load gets a fresh copy.
        //    Pinned maps (VPC_PODS, etc.) are reused from the shared pin dir.
        let mut pod_ebpf = EbpfLoader::new()
            .set_global("MY_GHOST_IPV6", &ipv6_bytes, true)
            .set_global("MY_GUEST_IPV4", &ip_host, true)
            .set_global("MY_VPC_ID", &vpc_id, true)
            .map_pin_path(BPFFS_PIN_DIR)
            .load(EBPF_BYTES)
            .with_context(|| format!("failed to load per-pod eBPF for {}", nk_name))?;

        // 3. Enter pod netns via setns() and attach SIIT programs to eth0
        //    Must use spawn_blocking to avoid affecting other async tasks on the tokio thread.
        let nk_name_owned = nk_name.to_string();
        let pid = container_pid;

        // Load SIIT programs before entering the netns (loading is kernel-global)
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

        // Enter pod netns, add clsact qdisc to eth0, attach SIIT with flipped directions
        tokio::task::spawn_blocking(move || -> Result<()> {
            let pod_ns_path = format!("/proc/{}/ns/net", pid);
            let pod_ns = std::fs::File::open(&pod_ns_path)
                .with_context(|| format!("failed to open pod netns at {}", pod_ns_path))?;
            let orig_ns = std::fs::File::open("/proc/self/ns/net")
                .context("failed to open current netns")?;

            // Enter pod network namespace
            let ret = unsafe { libc::setns(pod_ns.as_raw_fd(), libc::CLONE_NEWNET) };
            if ret != 0 {
                bail!(
                    "setns into pod netns failed: {}",
                    std::io::Error::last_os_error()
                );
            }

            // Add clsact qdisc to eth0 inside the pod
            let attach_result = (|| -> Result<()> {
                if let Err(e) = tc::qdisc_add_clsact("eth0") {
                    let msg = format!("{}", e);
                    if !msg.contains("exist") {
                        warn!("ebpf: failed to add clsact qdisc to eth0 in pod: {}", e);
                    }
                }

                // Attach siit_in → eth0 Egress (app sends IPv4 out → translated to IPv6)
                siit_in
                    .attach("eth0", TcAttachType::Egress)
                    .with_context(|| {
                        format!("failed to attach siit_in to eth0 egress in pod (nk={})", nk_name_owned)
                    })?;

                // Attach siit_out → eth0 Ingress (IPv6 arrives → translated to IPv4 for app)
                siit_out
                    .attach("eth0", TcAttachType::Ingress)
                    .with_context(|| {
                        format!("failed to attach siit_out to eth0 ingress in pod (nk={})", nk_name_owned)
                    })?;

                Ok(())
            })();

            // Always restore original netns before returning
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

        // 4. Add clsact qdisc to host-side netkit (for IPv6 classifiers)
        if let Err(e) = tc::qdisc_add_clsact(nk_name) {
            let msg = format!("{}", e);
            if !msg.contains("exist") {
                warn!("ebpf: failed to add clsact qdisc to {}: {}", nk_name, e);
            }
        }

        // 5. From main instance: attach IPv6 VPC classifiers on host-side netkit (already loaded, shared)
        let ingress_v6: &mut SchedClassifier = self
            .bpf
            .program_mut("tc_ingress_v6")
            .context("tc_ingress_v6 program not found")?
            .try_into()?;
        ingress_v6.load().ok(); // may already be loaded
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

        // 6. Store per-pod instance and cleanup info
        self.pod_bpf.insert(nk_name.to_string(), pod_ebpf);
        self.nk_vpc_pods
            .insert(nk_name.to_string(), (ip_host, vpc_id));

        debug!(
            "ebpf: installed SIIT in pod netns (pid={}) on eth0, IPv6 classifiers on nk={} ipv4={} ipv6={} vpc_id={}",
            container_pid, nk_name, guest_ipv4, ghost_ipv6, vpc_id
        );
        Ok(())
    }

    async fn remove_netkit_rules(&mut self, nk_name: &str) -> Result<()> {
        if let Some((ip_host, vpc_id)) = self.nk_vpc_pods.remove(nk_name) {
            // Remove VPC_PODS entry from shared pinned map
            let vpc_pod_key = VpcPodKey {
                vpc_id,
                _pad: 0,
                ipv4_addr: ip_host,
            };
            {
                let mut map: BpfHashMap<&mut aya::maps::MapData, VpcPodKey, VpcPodValue> =
                    BpfHashMap::try_from(
                        self.bpf
                            .map_mut("VPC_PODS")
                            .context("VPC_PODS map not found")?,
                    )?;
                map.remove(&vpc_pod_key).ok();
            }

            // Drop per-pod Ebpf instance — TC programs on eth0 inside the pod are cleaned
            // up automatically when Aya detaches on drop or when the pod netns is destroyed.
            self.pod_bpf.remove(nk_name);

            debug!("ebpf: removed netkit rules for nk={} (in-pod SIIT cleaned up automatically)", nk_name);
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
                // A → B
                let k1 = PeeringKey {
                    src_vpc_id: vpc_a.vpc_id,
                    dst_vpc_id: vpc_b.vpc_id,
                };
                map.insert(k1, allowed, 0)?;
                keys.push(k1);

                // B → A
                let k2 = PeeringKey {
                    src_vpc_id: vpc_b.vpc_id,
                    dst_vpc_id: vpc_a.vpc_id,
                };
                map.insert(k2, allowed, 0)?;
                keys.push(k2);
            }
            PeeringDirection::InitiatorOnly => {
                // A → B only
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
        let bridge_ifindex = Self::ifindex(bridge_name)?;

        // Populate NAT64_CONFIG[0]
        let config = Nat64Config {
            node_ipv4: node_ip,
            phys_ifindex,
            bridge_ifindex,
            _pad: 0,
        };
        let mut map: BpfArray<&mut aya::maps::MapData, Nat64Config> = BpfArray::try_from(
            self.bpf
                .map_mut("NAT64_CONFIG")
                .context("NAT64_CONFIG map not found")?,
        )?;
        map.set(0, config, 0)?;

        // Add clsact qdisc to bridge and physical interfaces
        for iface in [bridge_name, phys_name] {
            if let Err(e) = tc::qdisc_add_clsact(iface) {
                let msg = format!("{}", e);
                if !msg.contains("exist") {
                    warn!("ebpf: failed to add clsact qdisc to {}: {}", iface, e);
                }
            }
        }

        // Attach nat64_egress to bridge egress
        let egress: &mut SchedClassifier = self
            .bpf
            .program_mut("nat64_egress")
            .context("nat64_egress program not found")?
            .try_into()?;
        egress.load().ok();
        egress
            .attach(bridge_name, TcAttachType::Egress)
            .with_context(|| format!("failed to attach nat64_egress to {}", bridge_name))?;

        // Attach nat64_ingress to physical interface ingress
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
        // Programs remain attached but are harmless without config.
        // Clear the config map to disable translation.
        let config = Nat64Config {
            node_ipv4: 0,
            phys_ifindex: 0,
            bridge_ifindex: 0,
            _pad: 0,
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

        // Pod membership
        out.push_str("\n[VPC Membership]\n");
        let map: BpfHashMap<&aya::maps::MapData, PodKey, PodValue> = BpfHashMap::try_from(
            self.bpf
                .map("VPC_MEMBERSHIP")
                .context("VPC_MEMBERSHIP map not found")?,
        )?;
        for item in map.iter() {
            if let Ok((k, v)) = item {
                let addr = Ipv4Addr::from(k.ipv4_addr);
                out.push_str(&format!("  {} → vpc_id={}\n", addr, v.vpc_id));
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

        // Per-netkit SIIT instances
        out.push_str("\n[Per-Netkit SIIT Instances]\n");
        for (nk, (ipv4, vpc_id)) in &self.nk_vpc_pods {
            let addr = Ipv4Addr::from(*ipv4);
            out.push_str(&format!(
                "  nk={} ipv4={} vpc_id={}\n",
                nk, addr, vpc_id
            ));
        }

        // Per-TAP instances
        out.push_str("\n[Per-TAP Instances]\n");
        for (tap, (ipv4, vpc_id)) in &self.tap_to_ip {
            let addr = Ipv4Addr::from(*ipv4);
            out.push_str(&format!(
                "  tap={} ipv4={} vpc_id={}\n",
                tap, addr, vpc_id
            ));
        }

        // VPC Pods
        out.push_str("\n[VPC Pods]\n");
        if let Ok(map_data) = self.bpf.map("VPC_PODS") {
            if let Ok(map) =
                BpfHashMap::<&aya::maps::MapData, VpcPodKey, VpcPodValue>::try_from(map_data)
            {
                for item in map.iter() {
                    if let Ok((k, v)) = item {
                        let ipv4 = Ipv4Addr::from(k.ipv4_addr);
                        let ipv6 = Ipv6Addr::from(v.ghost_ipv6);
                        out.push_str(&format!(
                            "  vpc_id={} ipv4={} → ipv6={}\n",
                            k.vpc_id, ipv4, ipv6
                        ));
                    }
                }
            }
        }

        Ok(out)
    }

    async fn cleanup(&mut self) -> Result<()> {
        // Remove bpffs pin directory
        if Path::new(BPFFS_PIN_DIR).exists() {
            std::fs::remove_dir_all(BPFFS_PIN_DIR).ok();
        }

        self.active_vpcs.clear();
        self.pod_to_ip.clear();
        self.tap_to_ip.clear();
        self.peering_to_keys.clear();
        self.pod_bpf.clear();
        self.nk_vpc_pods.clear();
        self.tap_bpf.clear();

        info!("ebpf: cleaned up all state");
        Ok(())
    }
}
