use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::RwLock;
use tracing::{info, warn};

/// Lightweight embedded DNS server for service discovery.
///
/// Resolves `<service>.<namespace>.svc.cluster.local` → ClusterIP.
/// Uses a simple UDP-based DNS responder (no external dependencies).
pub struct DnsServer {
    /// FQDN → IP address mapping
    records: Arc<RwLock<HashMap<String, String>>>,
    listen_addr: SocketAddr,
    /// The domain suffix
    domain_suffix: String,
}

impl DnsServer {
    /// Create a new DNS server listening on the given address.
    pub fn new(listen_addr: SocketAddr) -> Self {
        Self {
            records: Arc::new(RwLock::new(HashMap::new())),
            listen_addr,
            domain_suffix: "svc.cluster.local".to_string(),
        }
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

    /// Start the DNS server as a background UDP listener.
    pub async fn start(&self) -> anyhow::Result<()> {
        info!("Starting embedded DNS server on {}", self.listen_addr);

        let socket = UdpSocket::bind(self.listen_addr).await?;
        let records = self.records.clone();

        tokio::spawn(async move {
            let mut buf = [0u8; 512];
            loop {
                match socket.recv_from(&mut buf).await {
                    Ok((len, src)) => {
                        if let Some(response) = Self::handle_dns_query(&buf[..len], &records).await
                        {
                            if let Err(e) = socket.send_to(&response, src).await {
                                warn!("DNS send error: {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        warn!("DNS recv error: {}", e);
                    }
                }
            }
        });

        info!("DNS server is running");
        Ok(())
    }

    /// Parse a minimal DNS query and generate a response.
    /// Supports A-record queries only (enough for service discovery).
    async fn handle_dns_query(
        query: &[u8],
        records: &Arc<RwLock<HashMap<String, String>>>,
    ) -> Option<Vec<u8>> {
        // Minimum DNS query is 12 bytes header + at least 1 byte question
        if query.len() < 13 {
            return None;
        }

        // Extract transaction ID (first 2 bytes)
        let txn_id = &query[0..2];

        // Parse the question name from the query
        let name = Self::parse_dns_name(query, 12)?;

        // Look up the name in our records
        let records_map = records.read().await;
        let ip = records_map.get(&name)?;

        // Parse IP into 4 octets
        let octets: Vec<u8> = ip.split('.').filter_map(|o| o.parse().ok()).collect();
        if octets.len() != 4 {
            return None;
        }

        // Build DNS response
        let mut response = Vec::with_capacity(64);

        // Header (12 bytes)
        response.extend_from_slice(txn_id); // Transaction ID
        response.extend_from_slice(&[0x81, 0x80]); // Flags: response, no error
        response.extend_from_slice(&[0x00, 0x01]); // QDCOUNT: 1
        response.extend_from_slice(&[0x00, 0x01]); // ANCOUNT: 1
        response.extend_from_slice(&[0x00, 0x00]); // NSCOUNT: 0
        response.extend_from_slice(&[0x00, 0x00]); // ARCOUNT: 0

        // Question section (copy from query)
        // Find the end of the question section
        let q_end = Self::find_question_end(query, 12)?;
        response.extend_from_slice(&query[12..q_end]);

        // Answer section
        response.extend_from_slice(&[0xC0, 0x0C]); // Name pointer to question
        response.extend_from_slice(&[0x00, 0x01]); // Type: A
        response.extend_from_slice(&[0x00, 0x01]); // Class: IN
        response.extend_from_slice(&[0x00, 0x00, 0x00, 0x3C]); // TTL: 60s
        response.extend_from_slice(&[0x00, 0x04]); // RDLENGTH: 4
        response.extend_from_slice(&octets); // RDATA: IP address

        Some(response)
    }

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

    /// Find the end of the question section (name + type + class).
    fn find_question_end(data: &[u8], mut offset: usize) -> Option<usize> {
        // Skip the name
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
        // Skip QTYPE (2 bytes) + QCLASS (2 bytes)
        offset += 4;
        if offset > data.len() {
            return None;
        }
        Some(offset)
    }
}
