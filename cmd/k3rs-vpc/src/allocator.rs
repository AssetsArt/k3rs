//! Ghost IPv6 Allocator — manages per-VPC IP pools and Ghost IPv6 construction.

use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Arc;

use anyhow::{bail, ensure};
use chrono::Utc;
use tracing::info;

use crate::store::{StoredAllocation, VpcStore};
use pkg_types::vpc::Vpc;

struct Allocation {
    guest_ipv4: Ipv4Addr,
    ghost_ipv6: Ipv6Addr,
}

struct VpcPool {
    vpc_id: u16,
    base_ip: u32,
    max_hosts: u32,
    next_offset: u32,
    allocations: HashMap<String, Allocation>,
}

pub struct AllocateResult {
    pub guest_ipv4: Ipv4Addr,
    pub ghost_ipv6: Ipv6Addr,
    pub vpc_id: u16,
}

pub struct QueryResult {
    pub guest_ipv4: Ipv4Addr,
    pub ghost_ipv6: Ipv6Addr,
    pub vpc_id: u16,
    pub vpc_name: String,
}

pub struct GhostAllocator {
    platform_prefix: u32,
    cluster_id: u32,
    pools: HashMap<String, VpcPool>,
    store: Arc<VpcStore>,
}

fn parse_cidr(cidr: &str) -> anyhow::Result<(u32, u8)> {
    let parts: Vec<&str> = cidr.split('/').collect();
    ensure!(parts.len() == 2, "invalid CIDR: {}", cidr);
    let addr: Ipv4Addr = parts[0].parse()?;
    let prefix_len: u8 = parts[1].parse()?;
    ensure!(prefix_len <= 30, "prefix_len {} too large for allocation", prefix_len);
    Ok((u32::from(addr), prefix_len))
}

impl GhostAllocator {
    pub fn new(platform_prefix: u32, cluster_id: u32, store: Arc<VpcStore>) -> Self {
        Self {
            platform_prefix,
            cluster_id,
            pools: HashMap::new(),
            store,
        }
    }

    pub fn store(&self) -> &VpcStore {
        &self.store
    }

    /// Rebuild in-memory pools from cached VPC definitions and persisted allocations.
    pub fn rebuild_pools(&mut self, vpcs: &[Vpc], stored: &[StoredAllocation]) {
        for vpc in vpcs {
            let (base_ip, prefix_len) = match parse_cidr(&vpc.ipv4_cidr) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("Skipping VPC {} with invalid CIDR {}: {}", vpc.name, vpc.ipv4_cidr, e);
                    continue;
                }
            };
            let max_hosts = (1u32 << (32 - prefix_len)) - 2;
            self.pools.insert(
                vpc.name.clone(),
                VpcPool {
                    vpc_id: vpc.vpc_id,
                    base_ip,
                    max_hosts,
                    next_offset: 1,
                    allocations: HashMap::new(),
                },
            );
        }

        // Restore persisted allocations into their pools
        for sa in stored {
            let pool = match self.pools.get_mut(&sa.vpc_name) {
                Some(p) => p,
                None => {
                    tracing::warn!("Stored allocation for unknown VPC {}, skipping", sa.vpc_name);
                    continue;
                }
            };
            let guest_ipv4: Ipv4Addr = match sa.guest_ipv4.parse() {
                Ok(ip) => ip,
                Err(e) => {
                    tracing::warn!("Invalid stored guest_ipv4 {}: {}", sa.guest_ipv4, e);
                    continue;
                }
            };
            let ghost_ipv6: Ipv6Addr = match sa.ghost_ipv6.parse() {
                Ok(ip) => ip,
                Err(e) => {
                    tracing::warn!("Invalid stored ghost_ipv6 {}: {}", sa.ghost_ipv6, e);
                    continue;
                }
            };
            let offset = u32::from(guest_ipv4) - pool.base_ip;
            if offset >= pool.next_offset {
                pool.next_offset = offset + 1;
            }
            pool.allocations.insert(
                sa.pod_id.clone(),
                Allocation {
                    guest_ipv4,
                    ghost_ipv6,
                },
            );
        }

        info!(
            "GhostAllocator rebuilt {} pools, {} total allocations",
            self.pools.len(),
            self.pools.values().map(|p| p.allocations.len()).sum::<usize>()
        );
    }

    /// Sync pool set with latest VPC definitions. Adds new VPCs, removes deleted ones
    /// (keeping existing allocations for VPCs that still exist).
    pub fn sync_vpcs(&mut self, vpcs: &[Vpc]) {
        let new_names: std::collections::HashSet<&str> =
            vpcs.iter().map(|v| v.name.as_str()).collect();

        // Remove pools for VPCs no longer in the list
        self.pools.retain(|name, _| new_names.contains(name.as_str()));

        // Add pools for new VPCs
        for vpc in vpcs {
            if self.pools.contains_key(&vpc.name) {
                continue;
            }
            let (base_ip, prefix_len) = match parse_cidr(&vpc.ipv4_cidr) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("Skipping VPC {} with invalid CIDR {}: {}", vpc.name, vpc.ipv4_cidr, e);
                    continue;
                }
            };
            let max_hosts = (1u32 << (32 - prefix_len)) - 2;
            self.pools.insert(
                vpc.name.clone(),
                VpcPool {
                    vpc_id: vpc.vpc_id,
                    base_ip,
                    max_hosts,
                    next_offset: 1,
                    allocations: HashMap::new(),
                },
            );
        }
    }

    /// Allocate a (GuestIPv4, GhostIPv6) pair for a pod. Idempotent: same pod_id returns same IP.
    pub async fn allocate(&mut self, pod_id: &str, vpc_name: &str) -> anyhow::Result<AllocateResult> {
        let pool = self
            .pools
            .get_mut(vpc_name)
            .ok_or_else(|| anyhow::anyhow!("VPC '{}' not found", vpc_name))?;

        // Idempotent: return existing allocation
        if let Some(existing) = pool.allocations.get(pod_id) {
            return Ok(AllocateResult {
                guest_ipv4: existing.guest_ipv4,
                ghost_ipv6: existing.ghost_ipv6,
                vpc_id: pool.vpc_id,
            });
        }

        // Find next available offset
        let allocated_offsets: std::collections::HashSet<u32> = pool
            .allocations
            .values()
            .map(|a| u32::from(a.guest_ipv4) - pool.base_ip)
            .collect();

        let mut offset = pool.next_offset;
        let mut tried = 0u32;
        loop {
            ensure!(tried < pool.max_hosts, "VPC '{}' pool exhausted", vpc_name);
            // Valid host offsets: 1..=max_hosts
            if offset < 1 || offset > pool.max_hosts {
                offset = 1;
            }
            if !allocated_offsets.contains(&offset) {
                break;
            }
            offset += 1;
            tried += 1;
        }

        let guest_ip_u32 = pool.base_ip + offset;
        let guest_ipv4 = Ipv4Addr::from(guest_ip_u32);
        let ghost_ipv6 = pkg_vpc::ghost_ipv6::construct(
            self.platform_prefix,
            self.cluster_id,
            pool.vpc_id,
            guest_ipv4,
        );

        let now = Utc::now();

        // Persist to store
        self.store
            .save_allocation(&StoredAllocation {
                pod_id: pod_id.to_string(),
                vpc_name: vpc_name.to_string(),
                guest_ipv4: guest_ipv4.to_string(),
                ghost_ipv6: ghost_ipv6.to_string(),
                vpc_id: pool.vpc_id,
                allocated_at: now,
            })
            .await?;

        pool.next_offset = offset + 1;
        pool.allocations.insert(
            pod_id.to_string(),
            Allocation {
                guest_ipv4,
                ghost_ipv6,
            },
        );

        Ok(AllocateResult {
            guest_ipv4,
            ghost_ipv6,
            vpc_id: pool.vpc_id,
        })
    }

    /// Release a pod's allocation from a VPC pool.
    pub async fn release(&mut self, pod_id: &str, vpc_name: &str) -> anyhow::Result<()> {
        let pool = self
            .pools
            .get_mut(vpc_name)
            .ok_or_else(|| anyhow::anyhow!("VPC '{}' not found", vpc_name))?;

        if pool.allocations.remove(pod_id).is_none() {
            bail!("No allocation for pod '{}' in VPC '{}'", pod_id, vpc_name);
        }

        self.store.delete_allocation(vpc_name, pod_id).await?;
        Ok(())
    }

    /// Return all allocations for a given VPC by vpc_id.
    /// Returns Vec<(pod_id, guest_ipv4, ghost_ipv6)>.
    pub fn get_routes(&self, vpc_id: u16) -> Vec<(String, String, String)> {
        for (_vpc_name, pool) in &self.pools {
            if pool.vpc_id == vpc_id {
                return pool
                    .allocations
                    .iter()
                    .map(|(pod_id, alloc)| {
                        (
                            pod_id.clone(),
                            alloc.guest_ipv4.to_string(),
                            alloc.ghost_ipv6.to_string(),
                        )
                    })
                    .collect();
            }
        }
        Vec::new()
    }

    /// Query a pod's allocation across all VPC pools.
    pub fn query(&self, pod_id: &str) -> Option<QueryResult> {
        for (vpc_name, pool) in &self.pools {
            if let Some(alloc) = pool.allocations.get(pod_id) {
                return Some(QueryResult {
                    guest_ipv4: alloc.guest_ipv4,
                    ghost_ipv6: alloc.ghost_ipv6,
                    vpc_id: pool.vpc_id,
                    vpc_name: vpc_name.clone(),
                });
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_cidr() {
        let (base, prefix) = parse_cidr("10.42.0.0/16").unwrap();
        assert_eq!(base, 0x0a2a0000);
        assert_eq!(prefix, 16);

        let (base, prefix) = parse_cidr("192.168.1.0/24").unwrap();
        assert_eq!(base, 0xc0a80100);
        assert_eq!(prefix, 24);
    }

    #[test]
    fn test_parse_cidr_invalid() {
        assert!(parse_cidr("not-a-cidr").is_err());
        assert!(parse_cidr("10.0.0.0/31").is_err());
        assert!(parse_cidr("10.0.0.0/32").is_err());
    }
}
