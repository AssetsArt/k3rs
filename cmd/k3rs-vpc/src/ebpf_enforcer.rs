//! EbpfEnforcer — eBPF-based network enforcement backend using TC classifiers.
//!
//! Uses Aya to load TC classifier programs and manage BPF hash maps for
//! VPC membership, CIDR info, and peering relationships.

use std::collections::{HashMap, HashSet};
use std::net::Ipv4Addr;
use std::path::Path;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use aya::Ebpf;
use aya::maps::hash_map::HashMap as BpfHashMap;
use aya::programs::tc::{self, SchedClassifier, TcAttachType};
use tracing::{debug, info, warn};

use k3rs_vpc::enforcer::NetworkEnforcer;
use k3rs_vpc_common::{PeeringKey, PeeringValue, PodKey, PodValue, VpcCidrKey, VpcCidrValue};
use pkg_types::vpc::{PeeringDirection, PeeringStatus, Vpc, VpcPeering};

// Safety: these #[repr(C)] types are Copy + 'static with no padding issues.
unsafe impl aya::Pod for PodKey {}
unsafe impl aya::Pod for PodValue {}
unsafe impl aya::Pod for VpcCidrKey {}
unsafe impl aya::Pod for VpcCidrValue {}
unsafe impl aya::Pod for PeeringKey {}
unsafe impl aya::Pod for PeeringValue {}

const BPFFS_PIN_DIR: &str = "/sys/fs/bpf/k3rs_vpc";

pub struct EbpfEnforcer {
    bpf: Ebpf,
    active_vpcs: HashSet<u16>,
    attached_interfaces: HashSet<String>,
    /// Reverse lookup: pod_id → (ipv4 in host byte order, vpc_id)
    pod_to_ip: HashMap<String, (u32, u16)>,
    /// Reverse lookup: tap_name → (ipv4 in host byte order, vpc_id)
    tap_to_ip: HashMap<String, (u32, u16)>,
    /// Reverse lookup: peering_name → list of PeeringKey entries inserted
    peering_to_keys: HashMap<String, Vec<PeeringKey>>,
}

impl EbpfEnforcer {
    /// Try to create a new EbpfEnforcer. Returns Err if eBPF is not available.
    pub fn new() -> Result<Self> {
        // Check basic eBPF support
        if !Path::new("/sys/fs/bpf").exists() {
            bail!("bpffs not mounted at /sys/fs/bpf");
        }

        // Load the compiled eBPF program bytes
        // aya-build compiles the eBPF crate and embeds the ELF via include_bytes_aligned!
        let bpf = Ebpf::load(include_bytes!(concat!(env!("OUT_DIR"), "/k3rs-vpc-ebpf")))
            .context("failed to load eBPF programs")?;

        Ok(Self {
            bpf,
            active_vpcs: HashSet::new(),
            attached_interfaces: HashSet::new(),
            pod_to_ip: HashMap::new(),
            tap_to_ip: HashMap::new(),
            peering_to_keys: HashMap::new(),
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

    /// Attach TC classifiers to an interface (IPv4 + IPv6, both ingress and egress).
    fn attach_tc(&mut self, interface: &str) -> Result<()> {
        if self.attached_interfaces.contains(interface) {
            return Ok(());
        }

        // Add clsact qdisc (required for TC programs)
        if let Err(e) = tc::qdisc_add_clsact(interface) {
            // Ignore "already exists" errors
            let msg = format!("{}", e);
            if !msg.contains("exist") {
                warn!("ebpf: failed to add clsact qdisc to {}: {}", interface, e);
            }
        }

        // Attach IPv4 egress
        let egress_prog: &mut SchedClassifier = self
            .bpf
            .program_mut("tc_egress")
            .context("tc_egress program not found")?
            .try_into()?;
        egress_prog.load().ok(); // may already be loaded
        egress_prog
            .attach(interface, TcAttachType::Egress)
            .with_context(|| format!("failed to attach tc_egress to {}", interface))?;

        // Attach IPv4 ingress
        let ingress_prog: &mut SchedClassifier = self
            .bpf
            .program_mut("tc_ingress")
            .context("tc_ingress program not found")?
            .try_into()?;
        ingress_prog.load().ok();
        ingress_prog
            .attach(interface, TcAttachType::Ingress)
            .with_context(|| format!("failed to attach tc_ingress to {}", interface))?;

        // Attach IPv6 egress (Ghost IPv6 native enforcement)
        let egress_v6: &mut SchedClassifier = self
            .bpf
            .program_mut("tc_egress_v6")
            .context("tc_egress_v6 program not found")?
            .try_into()?;
        egress_v6.load().ok();
        egress_v6
            .attach(interface, TcAttachType::Egress)
            .with_context(|| format!("failed to attach tc_egress_v6 to {}", interface))?;

        // Attach IPv6 ingress
        let ingress_v6: &mut SchedClassifier = self
            .bpf
            .program_mut("tc_ingress_v6")
            .context("tc_ingress_v6 program not found")?
            .try_into()?;
        ingress_v6.load().ok();
        ingress_v6
            .attach(interface, TcAttachType::Ingress)
            .with_context(|| format!("failed to attach tc_ingress_v6 to {}", interface))?;

        self.attached_interfaces.insert(interface.to_string());
        debug!(
            "ebpf: attached TC classifiers (v4+v6) to {}",
            interface
        );
        Ok(())
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

        // Insert into VPC_MEMBERSHIP map (same as pod rules)
        let key = PodKey { ipv4_addr: ip_host };
        let value = PodValue { vpc_id, _pad: 0 };

        let mut map: BpfHashMap<&mut aya::maps::MapData, PodKey, PodValue> = BpfHashMap::try_from(
            self.bpf
                .map_mut("VPC_MEMBERSHIP")
                .context("VPC_MEMBERSHIP map not found")?,
        )?;
        map.insert(key, value, 0)?;

        self.tap_to_ip
            .insert(tap_name.to_string(), (ip_host, vpc_id));

        // Attach TC classifiers to the TAP interface
        self.attach_tc(tap_name)?;

        debug!(
            "ebpf: installed TAP rules for tap={} ipv4={} vpc_id={}",
            tap_name, guest_ipv4, vpc_id
        );
        Ok(())
    }

    async fn remove_tap_rules(&mut self, tap_name: &str) -> Result<()> {
        if let Some((ip_host, _vpc_id)) = self.tap_to_ip.remove(tap_name) {
            let key = PodKey { ipv4_addr: ip_host };

            let mut map: BpfHashMap<&mut aya::maps::MapData, PodKey, PodValue> =
                BpfHashMap::try_from(
                    self.bpf
                        .map_mut("VPC_MEMBERSHIP")
                        .context("VPC_MEMBERSHIP map not found")?,
                )?;
            map.remove(&key).ok();

            debug!("ebpf: removed TAP rules for tap={}", tap_name);
        }
        // Note: TC programs remain attached — they're harmless without map entries.
        // Detaching would require tracking link IDs per-interface.
        Ok(())
    }

    async fn install_veth_rules(
        &mut self,
        veth_name: &str,
        guest_ipv4: &str,
        vpc_id: u16,
    ) -> Result<()> {
        let addr: Ipv4Addr = guest_ipv4.parse().context("invalid veth IPv4")?;
        let ip_host = u32::from(addr);

        // Insert into VPC_MEMBERSHIP map (same as pod/tap rules, for IPv4 path)
        let key = PodKey { ipv4_addr: ip_host };
        let value = PodValue { vpc_id, _pad: 0 };

        let mut map: BpfHashMap<&mut aya::maps::MapData, PodKey, PodValue> = BpfHashMap::try_from(
            self.bpf
                .map_mut("VPC_MEMBERSHIP")
                .context("VPC_MEMBERSHIP map not found")?,
        )?;
        map.insert(key, value, 0)?;

        self.tap_to_ip
            .insert(veth_name.to_string(), (ip_host, vpc_id));

        // Attach TC classifiers (IPv4 + IPv6) to the veth interface
        self.attach_tc(veth_name)?;

        debug!(
            "ebpf: installed veth rules for veth={} ipv4={} vpc_id={}",
            veth_name, guest_ipv4, vpc_id
        );
        Ok(())
    }

    async fn remove_veth_rules(&mut self, veth_name: &str) -> Result<()> {
        if let Some((ip_host, _vpc_id)) = self.tap_to_ip.remove(veth_name) {
            let key = PodKey { ipv4_addr: ip_host };

            let mut map: BpfHashMap<&mut aya::maps::MapData, PodKey, PodValue> =
                BpfHashMap::try_from(
                    self.bpf
                        .map_mut("VPC_MEMBERSHIP")
                        .context("VPC_MEMBERSHIP map not found")?,
                )?;
            map.remove(&key).ok();

            debug!("ebpf: removed veth rules for veth={}", veth_name);
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

        out.push_str(&format!(
            "\n[Attached interfaces]: {:?}\n",
            self.attached_interfaces
        ));

        Ok(out)
    }

    async fn cleanup(&mut self) -> Result<()> {
        // Remove bpffs pin directory
        if Path::new(BPFFS_PIN_DIR).exists() {
            std::fs::remove_dir_all(BPFFS_PIN_DIR).ok();
        }

        self.active_vpcs.clear();
        self.attached_interfaces.clear();
        self.pod_to_ip.clear();
        self.tap_to_ip.clear();
        self.peering_to_keys.clear();

        info!("ebpf: cleaned up all state");
        Ok(())
    }
}
