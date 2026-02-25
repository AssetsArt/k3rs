use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use tokio::sync::RwLock;
use tracing::info;

/// Lightweight CNI-like Pod network manager.
///
/// Allocates IP addresses from a configurable CIDR block (e.g. 10.42.0.0/16)
/// and maintains a mapping of pod_id → allocated IP.
pub struct PodNetwork {
    /// Base IP as a u32 (e.g. 10.42.0.0 → 0x0A2A0000)
    base_ip: u32,
    /// Subnet mask bit count
    _prefix_len: u8,
    /// Maximum number of addresses in this CIDR
    max_hosts: u32,
    /// Next IP offset to allocate (starts from 2 to skip network + gateway)
    next_offset: AtomicU32,
    /// pod_id → allocated IP string
    allocations: Arc<RwLock<HashMap<String, String>>>,
}

impl PodNetwork {
    /// Create a new PodNetwork with the given CIDR block (e.g. "10.42.0.0/16").
    pub fn new(cidr: &str) -> anyhow::Result<Self> {
        let parts: Vec<&str> = cidr.split('/').collect();
        if parts.len() != 2 {
            return Err(anyhow::anyhow!("Invalid CIDR format: {}", cidr));
        }

        let ip_str = parts[0];
        let prefix_len: u8 = parts[1]
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid prefix length: {}", parts[1]))?;

        let octets: Vec<u8> = ip_str
            .split('.')
            .map(|o| o.parse::<u8>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| anyhow::anyhow!("Invalid IP: {}", ip_str))?;

        if octets.len() != 4 {
            return Err(anyhow::anyhow!("Invalid IP: {}", ip_str));
        }

        let base_ip = (octets[0] as u32) << 24
            | (octets[1] as u32) << 16
            | (octets[2] as u32) << 8
            | (octets[3] as u32);

        let max_hosts = 1u32.checked_shl(32 - prefix_len as u32).unwrap_or(0);

        info!(
            "PodNetwork initialized: CIDR={}, max_hosts={}",
            cidr,
            max_hosts.saturating_sub(2)
        );

        Ok(Self {
            base_ip,
            _prefix_len: prefix_len,
            max_hosts,
            next_offset: AtomicU32::new(2), // Skip .0 (network) and .1 (gateway)
            allocations: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Allocate an IP for the given pod. Returns the allocated IP as a dotted-decimal string.
    pub async fn allocate_ip(&self, pod_id: &str) -> anyhow::Result<String> {
        // Check if already allocated
        {
            let map = self.allocations.read().await;
            if let Some(ip) = map.get(pod_id) {
                return Ok(ip.clone());
            }
        }

        let offset = self.next_offset.fetch_add(1, Ordering::Relaxed);
        if offset >= self.max_hosts {
            return Err(anyhow::anyhow!(
                "PodNetwork exhausted: no more IPs available"
            ));
        }

        let ip_u32 = self.base_ip + offset;
        let ip = format!(
            "{}.{}.{}.{}",
            (ip_u32 >> 24) & 0xFF,
            (ip_u32 >> 16) & 0xFF,
            (ip_u32 >> 8) & 0xFF,
            ip_u32 & 0xFF,
        );

        let mut map = self.allocations.write().await;
        map.insert(pod_id.to_string(), ip.clone());

        info!("PodNetwork: allocated {} → {}", pod_id, ip);
        Ok(ip)
    }

    /// Release a pod's IP allocation.
    pub async fn release_ip(&self, pod_id: &str) {
        let mut map = self.allocations.write().await;
        if let Some(ip) = map.remove(pod_id) {
            info!("PodNetwork: released {} (was {})", pod_id, ip);
        }
    }

    /// Look up a pod's allocated IP.
    pub async fn get_pod_ip(&self, pod_id: &str) -> Option<String> {
        let map = self.allocations.read().await;
        map.get(pod_id).cloned()
    }

    /// Dump all current allocations.
    pub async fn list_allocations(&self) -> HashMap<String, String> {
        let map = self.allocations.read().await;
        map.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_allocate_ip() {
        let net = PodNetwork::new("10.42.0.0/16").unwrap();
        let ip = net.allocate_ip("pod-1").await.unwrap();
        assert!(
            ip.starts_with("10.42."),
            "IP should be in 10.42.x.x range, got {}",
            ip
        );
    }

    #[tokio::test]
    async fn test_release_ip() {
        let net = PodNetwork::new("10.42.0.0/16").unwrap();
        let ip = net.allocate_ip("pod-1").await.unwrap();
        assert!(net.get_pod_ip("pod-1").await.is_some());
        net.release_ip("pod-1").await;
        assert!(net.get_pod_ip("pod-1").await.is_none());
        // ip was still valid before release
        assert!(!ip.is_empty());
    }

    #[tokio::test]
    async fn test_unique_allocations() {
        let net = PodNetwork::new("10.42.0.0/16").unwrap();
        let mut ips = Vec::new();
        for i in 0..10 {
            let ip = net.allocate_ip(&format!("pod-{}", i)).await.unwrap();
            ips.push(ip);
        }
        // All IPs should be unique
        let mut unique = ips.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            ips.len(),
            unique.len(),
            "All allocated IPs should be unique"
        );
    }

    #[tokio::test]
    async fn test_idempotent_allocation() {
        let net = PodNetwork::new("10.42.0.0/24").unwrap();
        let ip1 = net.allocate_ip("pod-1").await.unwrap();
        let ip2 = net.allocate_ip("pod-1").await.unwrap();
        assert_eq!(ip1, ip2, "Allocating same pod_id should return same IP");
    }
}
