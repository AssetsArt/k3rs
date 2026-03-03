use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::RwLock;
use tracing::{info, warn};

use pkg_types::vpc::{PeeringDirection, PeeringStatus, VpcPeering};

/// DNS query type: A record (IPv4).
const QTYPE_A: u16 = 1;

/// DNS query type: AAAA record (IPv6).
const QTYPE_AAAA: u16 = 28;

/// NAT64 well-known prefix (first 12 bytes): `64:ff9b::/96` (RFC 6052).
const NAT64_PREFIX: [u8; 12] = [0x00, 0x64, 0xff, 0x9b, 0, 0, 0, 0, 0, 0, 0, 0];

/// Ghost IPv6 address layout version.
const GHOST_VERSION: u8 = 1;

/// Default platform prefix: ULA encoding of "k3rs" → `fd6b:3372`.
const DEFAULT_PLATFORM_PREFIX: u32 = 0xfd6b_3372;

/// Default VPC ID for services without explicit VPC membership.
const DEFAULT_VPC_ID: u16 = 1;

/// Upstream forwarding timeout.
const UPSTREAM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Lightweight embedded DNS server with DNS64 support.
///
/// **Internal** (`*.svc.cluster.local`):
///   - AAAA queries → Ghost IPv6 (constructed from ClusterIP + VPC ID)
///   - A queries → ClusterIP (backward compat)
///
/// **External** (all other domains):
///   - AAAA queries → forward upstream as A, synthesize AAAA via `64:ff9b::/96` (DNS64)
///   - A queries → forward upstream, relay response as-is
pub struct DnsServer {
    /// FQDN → ClusterIP (non-VPC fallback)
    records: Arc<RwLock<HashMap<String, String>>>,
    /// FQDN → (ClusterIP, vpc_name, vpc_id) for VPC-scoped records
    vpc_records: Arc<RwLock<HashMap<String, (String, String, u16)>>>,
    /// pod_ip → vpc_name (source IP → VPC membership)
    vpc_members: Arc<RwLock<HashMap<String, String>>>,
    /// Directed peering pairs: (src_vpc, dst_vpc) — src can resolve dst's services
    peered_vpcs: Arc<RwLock<HashSet<(String, String)>>>,
    listen_addr: SocketAddr,
    domain_suffix: String,
    /// Ghost IPv6 platform prefix (e.g., 0xfd6b3372).
    platform_prefix: u32,
    /// Cluster ID for Ghost IPv6 construction.
    cluster_id: u32,
    /// Upstream DNS resolver for forwarding external queries.
    upstream: SocketAddr,
}

impl DnsServer {
    /// Create a new DNS server listening on the given address.
    pub fn new(listen_addr: SocketAddr) -> Self {
        Self {
            records: Arc::new(RwLock::new(HashMap::new())),
            vpc_records: Arc::new(RwLock::new(HashMap::new())),
            vpc_members: Arc::new(RwLock::new(HashMap::new())),
            peered_vpcs: Arc::new(RwLock::new(HashSet::new())),
            listen_addr,
            domain_suffix: "svc.cluster.local".to_string(),
            platform_prefix: DEFAULT_PLATFORM_PREFIX,
            cluster_id: 0,
            upstream: "8.8.8.8:53".parse().unwrap(),
        }
    }

    /// Configure Ghost IPv6 parameters for AAAA record construction.
    pub fn set_ghost_config(&mut self, platform_prefix: u32, cluster_id: u32) {
        self.platform_prefix = platform_prefix;
        self.cluster_id = cluster_id;
    }

    /// Set the upstream DNS resolver for forwarding external queries.
    pub fn set_upstream(&mut self, addr: SocketAddr) {
        self.upstream = addr;
    }

    /// Update DNS records from a list of Services.
    pub async fn update_records(&self, services: &[pkg_types::service::Service]) {
        let mut new_records = HashMap::new();

        for svc in services {
            if let Some(ref cluster_ip) = svc.cluster_ip {
                // <service-name>.<namespace>.svc.cluster.local
                let fqdn = format!("{}.{}.{}", svc.name, svc.namespace, self.domain_suffix);
                new_records.insert(fqdn, cluster_ip.clone());
            }
        }

        let count = new_records.len();
        let mut records = self.records.write().await;
        *records = new_records;
        info!("DNS records updated: {} entries", count);
    }

    /// Update VPC-scoped DNS records from services, pod IP → VPC mapping,
    /// and VPC name → VPC ID mapping.
    ///
    /// Builds VPC-tagged records so that DNS queries can be filtered by
    /// the source pod's VPC membership. Also updates the vpc_members map.
    pub async fn update_records_vpc(
        &self,
        services: &[pkg_types::service::Service],
        ip_to_vpc: &HashMap<String, String>,
        vpc_name_to_id: &HashMap<String, u16>,
    ) {
        let mut new_vpc_records = HashMap::new();

        for svc in services {
            if let Some(ref cluster_ip) = svc.cluster_ip {
                let fqdn = format!("{}.{}.{}", svc.name, svc.namespace, self.domain_suffix);
                let svc_vpc = svc.vpc.as_deref().unwrap_or("default").to_string();
                let vpc_id = vpc_name_to_id
                    .get(&svc_vpc)
                    .copied()
                    .unwrap_or(DEFAULT_VPC_ID);
                new_vpc_records.insert(fqdn, (cluster_ip.clone(), svc_vpc, vpc_id));
            }
        }

        let vpc_record_count = new_vpc_records.len();
        {
            let mut vr = self.vpc_records.write().await;
            *vr = new_vpc_records;
        }
        {
            let mut vm = self.vpc_members.write().await;
            *vm = ip_to_vpc.clone();
        }

        info!(
            "DNS VPC records updated: {} entries, {} pod-to-VPC mappings",
            vpc_record_count,
            ip_to_vpc.len()
        );
    }

    /// Update the set of peered VPC pairs from the latest peerings list.
    ///
    /// Bidirectional peerings insert both `(a,b)` and `(b,a)`.
    /// InitiatorOnly inserts only `(a,b)` — vpc_a can resolve vpc_b's services.
    pub async fn update_peerings(&self, peerings: &[VpcPeering]) {
        let mut pairs = HashSet::new();
        for p in peerings {
            if p.status != PeeringStatus::Active {
                continue;
            }
            match p.direction {
                PeeringDirection::Bidirectional => {
                    pairs.insert((p.vpc_a.clone(), p.vpc_b.clone()));
                    pairs.insert((p.vpc_b.clone(), p.vpc_a.clone()));
                }
                PeeringDirection::InitiatorOnly => {
                    pairs.insert((p.vpc_a.clone(), p.vpc_b.clone()));
                }
            }
        }
        let count = pairs.len();
        let mut pv = self.peered_vpcs.write().await;
        *pv = pairs;
        info!("DNS peered VPC pairs updated: {} directed pairs", count);
    }

    /// Load DNS records from a JSON file (for cache-based startup).
    /// Returns the number of records loaded.
    pub async fn load_from_file(&self, path: &str) -> anyhow::Result<usize> {
        let data = std::fs::read_to_string(path)?;
        let new_records: HashMap<String, String> = serde_json::from_str(&data)?;
        let count = new_records.len();
        let mut records = self.records.write().await;
        *records = new_records;
        Ok(count)
    }

    /// Start the DNS server as a background UDP listener.
    pub async fn start(&self) -> anyhow::Result<()> {
        info!("Starting embedded DNS server on {}", self.listen_addr);

        let socket = UdpSocket::bind(self.listen_addr).await?;
        let records = self.records.clone();
        let vpc_records = self.vpc_records.clone();
        let vpc_members = self.vpc_members.clone();
        let peered_vpcs = self.peered_vpcs.clone();
        let platform_prefix = self.platform_prefix;
        let cluster_id = self.cluster_id;
        let upstream = self.upstream;
        let domain_suffix = self.domain_suffix.clone();

        tokio::spawn(async move {
            let mut buf = [0u8; 512];
            loop {
                match socket.recv_from(&mut buf).await {
                    Ok((len, src)) => {
                        if let Some(response) = Self::handle_dns_query(
                            &buf[..len],
                            &records,
                            &vpc_records,
                            &vpc_members,
                            &peered_vpcs,
                            &src,
                            platform_prefix,
                            cluster_id,
                            upstream,
                            &domain_suffix,
                        )
                        .await
                            && let Err(e) = socket.send_to(&response, src).await
                        {
                            warn!("DNS send error: {}", e);
                        }
                    }
                    Err(e) => {
                        warn!("DNS recv error: {}", e);
                    }
                }
            }
        });

        info!("DNS server is running (DNS64 enabled, upstream={})", self.upstream);
        Ok(())
    }

    // ─── Query Handler ──────────────────────────────────────────────

    /// Parse a DNS query and generate the appropriate response.
    ///
    /// - A queries for internal services → ClusterIP (backward compat)
    /// - AAAA queries for internal services → Ghost IPv6 (from ClusterIP + VPC ID)
    /// - A queries for external domains → forward upstream
    /// - AAAA queries for external domains → DNS64 (forward as A, synthesize AAAA)
    #[allow(clippy::too_many_arguments)]
    async fn handle_dns_query(
        query: &[u8],
        records: &Arc<RwLock<HashMap<String, String>>>,
        vpc_records: &Arc<RwLock<HashMap<String, (String, String, u16)>>>,
        vpc_members: &Arc<RwLock<HashMap<String, String>>>,
        peered_vpcs: &Arc<RwLock<HashSet<(String, String)>>>,
        src: &SocketAddr,
        platform_prefix: u32,
        cluster_id: u32,
        upstream: SocketAddr,
        domain_suffix: &str,
    ) -> Option<Vec<u8>> {
        // Minimum DNS query: 12-byte header + at least 1 byte question
        if query.len() < 13 {
            return None;
        }

        let txn_id = &query[0..2];
        let name = Self::parse_dns_name(query, 12)?;

        // Parse QTYPE from the question section
        let name_end = Self::find_name_end(query, 12)?;
        if name_end + 4 > query.len() {
            return None;
        }
        let qtype = u16::from_be_bytes([query[name_end], query[name_end + 1]]);
        let q_end = name_end + 4; // QTYPE (2) + QCLASS (2)

        let is_internal = name.ends_with(domain_suffix);

        if is_internal {
            // Determine the source pod's VPC (if any)
            let src_ip = src.ip().to_string();
            let members = vpc_members.read().await;
            let src_vpc = members.get(&src_ip).cloned();
            drop(members);

            // Resolve internal service (returns ClusterIP + VPC ID)
            let (ip_str, vpc_id) = Self::resolve_internal(
                &name,
                &src_vpc,
                records,
                vpc_records,
                peered_vpcs,
            )
            .await?;

            let octets: Vec<u8> = ip_str.split('.').filter_map(|o| o.parse().ok()).collect();
            if octets.len() != 4 {
                return None;
            }

            let question = &query[12..q_end];

            match qtype {
                QTYPE_A => Self::build_a_response(txn_id, question, &octets),
                QTYPE_AAAA => {
                    let ghost = Self::construct_ghost_ipv6(
                        platform_prefix,
                        cluster_id,
                        vpc_id,
                        &octets,
                    );
                    Self::build_aaaa_response(txn_id, question, &ghost)
                }
                _ => None,
            }
        } else {
            // External domain — forward upstream
            match qtype {
                QTYPE_AAAA => {
                    // DNS64: forward as A query, synthesize AAAA from response
                    Self::dns64_forward(query, name_end, q_end, upstream).await
                }
                _ => {
                    // Forward as-is (A queries, etc.)
                    Self::forward_upstream(query, upstream).await
                }
            }
        }
    }

    // ─── Internal Resolution ────────────────────────────────────────

    /// Resolve an internal service name with VPC-scoped access control.
    /// Returns `(ClusterIP, vpc_id)` if allowed, `None` if denied or not found.
    async fn resolve_internal(
        name: &str,
        src_vpc: &Option<String>,
        records: &Arc<RwLock<HashMap<String, String>>>,
        vpc_records: &Arc<RwLock<HashMap<String, (String, String, u16)>>>,
        peered_vpcs: &Arc<RwLock<HashSet<(String, String)>>>,
    ) -> Option<(String, u16)> {
        if let Some(source_vpc) = src_vpc {
            // VPC-scoped resolution
            let vr = vpc_records.read().await;
            if let Some((ip, svc_vpc, vpc_id)) = vr.get(name) {
                if svc_vpc == source_vpc {
                    // Same VPC — allow
                    Some((ip.clone(), *vpc_id))
                } else {
                    // Check peering
                    let pv = peered_vpcs.read().await;
                    if pv.contains(&(source_vpc.clone(), svc_vpc.clone())) {
                        Some((ip.clone(), *vpc_id))
                    } else {
                        // Not peered — deny
                        None
                    }
                }
            } else {
                // Not in VPC records, fall back to plain records
                let records_map = records.read().await;
                records_map
                    .get(name)
                    .map(|ip| (ip.clone(), DEFAULT_VPC_ID))
            }
        } else {
            // Non-VPC source — use plain records (backward compat)
            let records_map = records.read().await;
            records_map
                .get(name)
                .map(|ip| (ip.clone(), DEFAULT_VPC_ID))
        }
    }

    // ─── Ghost IPv6 Construction ────────────────────────────────────

    /// Construct a Ghost IPv6 address from ClusterIP + VPC metadata.
    ///
    /// Layout (128 bits):
    ///   b[0..4]   = platform_prefix (BE)
    ///   b[4]      = (version << 4) | 0
    ///   b[5]      = 0
    ///   b[6..8]   = cluster_id high 16
    ///   b[8..10]  = cluster_id low 16
    ///   b[10..12] = vpc_id
    ///   b[12..16] = IPv4 octets
    fn construct_ghost_ipv6(
        platform_prefix: u32,
        cluster_id: u32,
        vpc_id: u16,
        ipv4: &[u8],
    ) -> [u8; 16] {
        let mut b = [0u8; 16];
        b[0..4].copy_from_slice(&platform_prefix.to_be_bytes());
        b[4] = GHOST_VERSION << 4;
        b[5] = 0x00;
        let cluster_hi = (cluster_id >> 16) as u16;
        let cluster_lo = (cluster_id & 0xFFFF) as u16;
        b[6..8].copy_from_slice(&cluster_hi.to_be_bytes());
        b[8..10].copy_from_slice(&cluster_lo.to_be_bytes());
        b[10..12].copy_from_slice(&vpc_id.to_be_bytes());
        b[12..16].copy_from_slice(ipv4);
        b
    }

    // ─── DNS Response Builders ──────────────────────────────────────

    /// Build a DNS response with an A record (4-byte IPv4 RDATA).
    fn build_a_response(txn_id: &[u8], question: &[u8], ip: &[u8]) -> Option<Vec<u8>> {
        let mut r = Vec::with_capacity(64);
        // Header
        r.extend_from_slice(txn_id);
        r.extend_from_slice(&[0x81, 0x80]); // Flags: response, no error
        r.extend_from_slice(&[0x00, 0x01]); // QDCOUNT: 1
        r.extend_from_slice(&[0x00, 0x01]); // ANCOUNT: 1
        r.extend_from_slice(&[0x00, 0x00]); // NSCOUNT: 0
        r.extend_from_slice(&[0x00, 0x00]); // ARCOUNT: 0
        // Question section
        r.extend_from_slice(question);
        // Answer section
        r.extend_from_slice(&[0xC0, 0x0C]); // Name pointer to question
        r.extend_from_slice(&[0x00, 0x01]); // Type: A
        r.extend_from_slice(&[0x00, 0x01]); // Class: IN
        r.extend_from_slice(&[0x00, 0x00, 0x00, 0x3C]); // TTL: 60s
        r.extend_from_slice(&[0x00, 0x04]); // RDLENGTH: 4
        r.extend_from_slice(ip);
        Some(r)
    }

    /// Build a DNS response with an AAAA record (16-byte IPv6 RDATA).
    fn build_aaaa_response(txn_id: &[u8], question: &[u8], ipv6: &[u8; 16]) -> Option<Vec<u8>> {
        let mut r = Vec::with_capacity(80);
        // Header
        r.extend_from_slice(txn_id);
        r.extend_from_slice(&[0x81, 0x80]); // Flags: response, no error
        r.extend_from_slice(&[0x00, 0x01]); // QDCOUNT: 1
        r.extend_from_slice(&[0x00, 0x01]); // ANCOUNT: 1
        r.extend_from_slice(&[0x00, 0x00]); // NSCOUNT: 0
        r.extend_from_slice(&[0x00, 0x00]); // ARCOUNT: 0
        // Question section
        r.extend_from_slice(question);
        // Answer section
        r.extend_from_slice(&[0xC0, 0x0C]); // Name pointer to question
        r.extend_from_slice(&[0x00, 0x1C]); // Type: AAAA (28)
        r.extend_from_slice(&[0x00, 0x01]); // Class: IN
        r.extend_from_slice(&[0x00, 0x00, 0x00, 0x3C]); // TTL: 60s
        r.extend_from_slice(&[0x00, 0x10]); // RDLENGTH: 16
        r.extend_from_slice(ipv6);
        Some(r)
    }

    // ─── DNS64 / Upstream Forwarding ────────────────────────────────

    /// DNS64: forward an AAAA query as an A query upstream, then synthesize
    /// an AAAA response using the NAT64 prefix `64:ff9b::/96`.
    async fn dns64_forward(
        query: &[u8],
        name_end: usize,
        q_end: usize,
        upstream: SocketAddr,
    ) -> Option<Vec<u8>> {
        // Build A query from the original AAAA query
        let mut a_query = query.to_vec();
        a_query[name_end] = 0x00;
        a_query[name_end + 1] = 0x01; // QTYPE = A

        // Forward to upstream
        let response = Self::forward_upstream(&a_query, upstream).await?;

        // Extract first A record IP from upstream response
        let ipv4 = Self::extract_a_record_ip(&response)?;

        // Synthesize NAT64 AAAA: 64:ff9b::<ipv4>
        let mut aaaa = [0u8; 16];
        aaaa[..12].copy_from_slice(&NAT64_PREFIX);
        aaaa[12..16].copy_from_slice(&ipv4);

        // Build AAAA response using the original transaction ID and question section
        Self::build_aaaa_response(&query[0..2], &query[12..q_end], &aaaa)
    }

    /// Forward a raw DNS query to an upstream resolver and return the response.
    async fn forward_upstream(query: &[u8], upstream: SocketAddr) -> Option<Vec<u8>> {
        let sock = UdpSocket::bind("0.0.0.0:0").await.ok()?;
        sock.send_to(query, upstream).await.ok()?;

        let mut buf = [0u8; 512];
        match tokio::time::timeout(UPSTREAM_TIMEOUT, sock.recv_from(&mut buf)).await {
            Ok(Ok((len, _))) => Some(buf[..len].to_vec()),
            _ => None,
        }
    }

    /// Extract the first A record's IPv4 address from a DNS response.
    fn extract_a_record_ip(response: &[u8]) -> Option<[u8; 4]> {
        if response.len() < 12 {
            return None;
        }

        // Check RCODE (lower 4 bits of byte 3) — must be 0 (no error)
        if response[3] & 0x0F != 0 {
            return None;
        }

        let qdcount = u16::from_be_bytes([response[4], response[5]]);
        let ancount = u16::from_be_bytes([response[6], response[7]]);
        if ancount == 0 {
            return None;
        }

        // Skip past the question section
        let mut offset = 12;
        for _ in 0..qdcount {
            offset = Self::find_name_end(response, offset)?;
            offset += 4; // QTYPE + QCLASS
        }

        // Iterate answers to find the first A record
        for _ in 0..ancount {
            offset = Self::find_name_end(response, offset)?;
            if offset + 10 > response.len() {
                return None;
            }

            let rtype = u16::from_be_bytes([response[offset], response[offset + 1]]);
            offset += 2; // TYPE
            offset += 2; // CLASS
            offset += 4; // TTL
            let rdlength = u16::from_be_bytes([response[offset], response[offset + 1]]) as usize;
            offset += 2; // RDLENGTH

            if offset + rdlength > response.len() {
                return None;
            }

            if rtype == QTYPE_A && rdlength == 4 {
                let mut ip = [0u8; 4];
                ip.copy_from_slice(&response[offset..offset + 4]);
                return Some(ip);
            }

            offset += rdlength; // skip this answer's RDATA
        }

        None
    }

    // ─── DNS Packet Parsing ─────────────────────────────────────────

    /// Parse a DNS name from a raw DNS packet at the given offset.
    fn parse_dns_name(data: &[u8], mut offset: usize) -> Option<String> {
        let mut parts = Vec::new();

        loop {
            if offset >= data.len() {
                return None;
            }

            let len = data[offset] as usize;
            if len == 0 {
                break;
            }

            // Check for pointer (compression)
            if len & 0xC0 == 0xC0 {
                break;
            }

            offset += 1;
            if offset + len > data.len() {
                return None;
            }

            let part = String::from_utf8_lossy(&data[offset..offset + len]).to_string();
            parts.push(part);
            offset += len;
        }

        if parts.is_empty() {
            None
        } else {
            Some(parts.join("."))
        }
    }

    /// Find the byte offset immediately after a DNS name (before QTYPE/QCLASS).
    fn find_name_end(data: &[u8], mut offset: usize) -> Option<usize> {
        loop {
            if offset >= data.len() {
                return None;
            }
            let len = data[offset] as usize;
            if len == 0 {
                offset += 1;
                break;
            }
            if len & 0xC0 == 0xC0 {
                offset += 2;
                break;
            }
            offset += 1 + len;
        }
        Some(offset)
    }

}
