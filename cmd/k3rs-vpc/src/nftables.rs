//! nftables rule management engine for VPC isolation enforcement.
//!
//! Manages `table inet k3rs_vpc` with per-VPC ingress/egress chains,
//! per-pod forwarding rules, anti-spoofing, and TAP interface rules.
//! Uses `nft` CLI for rule manipulation (no library dependency).
//! Multi-command operations use atomic `nft -f` batching for performance.

use std::collections::HashSet;

use anyhow::{Context, Result};
use async_trait::async_trait;
use tracing::{debug, info, warn};

use k3rs_vpc::enforcer::NetworkEnforcer;
use k3rs_vpc::store::StoredAllocation;
use pkg_types::vpc::{PeeringDirection, PeeringStatus, Vpc, VpcPeering};

const TABLE_NAME: &str = "inet k3rs_vpc";

pub struct NftManager {
    /// Tracks which VPC chains exist (to avoid duplicate creation)
    active_vpc_chains: HashSet<u16>,
}

impl NftManager {
    pub fn new() -> Self {
        Self {
            active_vpc_chains: HashSet::new(),
        }
    }
}

#[async_trait]
impl NetworkEnforcer for NftManager {
    fn name(&self) -> &str {
        "nftables"
    }

    /// Create `table inet k3rs_vpc` with the base `forward` chain and `input_validation` chain.
    /// Idempotent (uses `add` not `create`).
    async fn init(&mut self) -> Result<()> {
        run_nft_batch(&[
            &format!("add table {}", TABLE_NAME),
            &format!(
                "add chain {} forward {{ type filter hook forward priority 0; policy accept; }}",
                TABLE_NAME
            ),
            &format!(
                "add chain {} input_validation {{ type filter hook input priority -1; policy accept; }}",
                TABLE_NAME
            ),
        ])
        .await?;

        info!("nftables: initialized table {}", TABLE_NAME);
        Ok(())
    }

    /// Create per-VPC ingress and egress chains with intra-VPC accept + default drop.
    /// Idempotent — tracks in `active_vpc_chains`.
    async fn ensure_vpc(&mut self, vpc_id: u16, cidr: &str) -> Result<()> {
        if self.active_vpc_chains.contains(&vpc_id) {
            return Ok(());
        }

        let ingress = format!("vpc_{}_ingress", vpc_id);
        let egress = format!("vpc_{}_egress", vpc_id);
        let intra_rule = format!("ip saddr {} ip daddr {} accept", cidr, cidr);

        run_nft_batch(&[
            &format!("add chain {} {}", TABLE_NAME, ingress),
            &format!("add chain {} {}", TABLE_NAME, egress),
            &format!("add rule {} {} {}", TABLE_NAME, ingress, intra_rule),
            &format!("add rule {} {} {}", TABLE_NAME, egress, intra_rule),
            &format!("add rule {} {} drop", TABLE_NAME, ingress),
            &format!("add rule {} {} drop", TABLE_NAME, egress),
        ])
        .await?;

        self.active_vpc_chains.insert(vpc_id);
        info!(
            "nftables: created VPC chains for vpc_id={} cidr={}",
            vpc_id, cidr
        );
        Ok(())
    }

    /// Delete VPC chains when a VPC is removed.
    async fn remove_vpc(&mut self, vpc_id: u16) -> Result<()> {
        if !self.active_vpc_chains.remove(&vpc_id) {
            return Ok(());
        }

        let ingress = format!("vpc_{}_ingress", vpc_id);
        let egress = format!("vpc_{}_egress", vpc_id);

        // Flush then delete (flush removes rules so delete succeeds).
        // Each pair must be sequential (flush before delete), but we can batch all four.
        run_nft_batch(&[
            &format!("flush chain {} {}", TABLE_NAME, ingress),
            &format!("delete chain {} {}", TABLE_NAME, ingress),
            &format!("flush chain {} {}", TABLE_NAME, egress),
            &format!("delete chain {} {}", TABLE_NAME, egress),
        ])
        .await
        .ok();

        info!("nftables: removed VPC chains for vpc_id={}", vpc_id);
        Ok(())
    }

    /// Install forwarding rules and anti-spoofing for a pod.
    /// Uses `comment "pod:<pod_id>"` for targeted removal.
    async fn install_pod_rules(
        &mut self,
        pod_id: &str,
        guest_ipv4: &str,
        vpc_id: u16,
    ) -> Result<()> {
        let comment = format!("pod:{}", pod_id);
        let egress_chain = format!("vpc_{}_egress", vpc_id);
        let ingress_chain = format!("vpc_{}_ingress", vpc_id);

        run_nft_batch(&[
            // Forward chain: jump to VPC egress for traffic FROM this pod
            &format!(
                "add rule {} forward ip saddr {} jump {} comment \"{}\"",
                TABLE_NAME, guest_ipv4, egress_chain, comment
            ),
            // Forward chain: jump to VPC ingress for traffic TO this pod
            &format!(
                "add rule {} forward ip daddr {} jump {} comment \"{}\"",
                TABLE_NAME, guest_ipv4, ingress_chain, comment
            ),
            // Anti-spoofing: drop packets from TAP interfaces claiming this pod's IP
            &format!(
                "add rule {} input_validation iifname \"tap-*\" ip saddr != {} drop comment \"{}\"",
                TABLE_NAME, guest_ipv4, comment
            ),
        ])
        .await?;

        debug!(
            "nftables: installed pod rules for pod={} ipv4={} vpc_id={}",
            pod_id, guest_ipv4, vpc_id
        );
        Ok(())
    }

    /// Remove all rules with comment `pod:<pod_id>` by listing + deleting by handle.
    async fn remove_pod_rules(&mut self, pod_id: &str) -> Result<()> {
        let comment = format!("pod:{}", pod_id);
        remove_rules_by_comment(&comment).await?;
        debug!("nftables: removed pod rules for pod={}", pod_id);
        Ok(())
    }

    /// Install TAP-specific rules matching iifname/oifname on the TAP device.
    /// Uses `comment "tap:<tap_name>"`.
    async fn install_tap_rules(
        &mut self,
        tap_name: &str,
        guest_ipv4: &str,
        vpc_id: u16,
    ) -> Result<()> {
        let comment = format!("tap:{}", tap_name);
        let egress_chain = format!("vpc_{}_egress", vpc_id);
        let ingress_chain = format!("vpc_{}_ingress", vpc_id);

        run_nft_batch(&[
            // Traffic leaving the TAP (from VM) → VPC egress chain
            &format!(
                "add rule {} forward iifname \"{}\" ip saddr {} jump {} comment \"{}\"",
                TABLE_NAME, tap_name, guest_ipv4, egress_chain, comment
            ),
            // Traffic entering the TAP (to VM) → VPC ingress chain
            &format!(
                "add rule {} forward oifname \"{}\" ip daddr {} jump {} comment \"{}\"",
                TABLE_NAME, tap_name, guest_ipv4, ingress_chain, comment
            ),
        ])
        .await?;

        debug!(
            "nftables: installed TAP rules for tap={} ipv4={} vpc_id={}",
            tap_name, guest_ipv4, vpc_id
        );
        Ok(())
    }

    /// Remove all rules with comment `tap:<tap_name>`.
    async fn remove_tap_rules(&mut self, tap_name: &str) -> Result<()> {
        let comment = format!("tap:{}", tap_name);
        remove_rules_by_comment(&comment).await?;
        debug!("nftables: removed TAP rules for tap={}", tap_name);
        Ok(())
    }

    async fn install_veth_rules(
        &mut self,
        veth_name: &str,
        guest_ipv4: &str,
        _ghost_ipv6: &str,
        vpc_id: u16,
    ) -> Result<()> {
        // nftables enforcer: veth rules use the same pattern as TAP rules (no SIIT)
        self.install_tap_rules(veth_name, guest_ipv4, vpc_id).await
    }

    async fn remove_veth_rules(&mut self, veth_name: &str) -> Result<()> {
        self.remove_tap_rules(veth_name).await
    }

    /// Install cross-VPC accept rules for a peering relationship.
    ///
    /// Uses `insert rule` so rules go before the drop at end of chain.
    /// All rules are tagged with comment `peering:<name>` for targeted removal.
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
                    "nftables: peering '{}' references unknown VPC(s) ({}, {})",
                    peering.name, peering.vpc_a, peering.vpc_b
                );
                return Ok(());
            }
        };

        let comment = format!("peering:{}", peering.name);
        let id_a = vpc_a.vpc_id;
        let id_b = vpc_b.vpc_id;
        let cidr_a = &vpc_a.ipv4_cidr;
        let cidr_b = &vpc_b.ipv4_cidr;

        let cmds: Vec<String> = match peering.direction {
            PeeringDirection::Bidirectional => vec![
                // A ingress: accept from B
                format!(
                    "insert rule {} vpc_{}_ingress ip saddr {} accept comment \"{}\"",
                    TABLE_NAME, id_a, cidr_b, comment
                ),
                // A egress: accept to B
                format!(
                    "insert rule {} vpc_{}_egress ip daddr {} accept comment \"{}\"",
                    TABLE_NAME, id_a, cidr_b, comment
                ),
                // B ingress: accept from A
                format!(
                    "insert rule {} vpc_{}_ingress ip saddr {} accept comment \"{}\"",
                    TABLE_NAME, id_b, cidr_a, comment
                ),
                // B egress: accept to A
                format!(
                    "insert rule {} vpc_{}_egress ip daddr {} accept comment \"{}\"",
                    TABLE_NAME, id_b, cidr_a, comment
                ),
            ],
            PeeringDirection::InitiatorOnly => vec![
                // A egress: accept to B
                format!(
                    "insert rule {} vpc_{}_egress ip daddr {} accept comment \"{}\"",
                    TABLE_NAME, id_a, cidr_b, comment
                ),
                // B ingress: accept from A
                format!(
                    "insert rule {} vpc_{}_ingress ip saddr {} accept comment \"{}\"",
                    TABLE_NAME, id_b, cidr_a, comment
                ),
            ],
        };

        let cmd_refs: Vec<&str> = cmds.iter().map(|s| s.as_str()).collect();
        run_nft_batch(&cmd_refs).await?;

        info!(
            "nftables: installed peering rules for '{}' ({} <-> {} {:?})",
            peering.name, peering.vpc_a, peering.vpc_b, peering.direction
        );
        Ok(())
    }

    /// Remove all nftables rules associated with a peering by name.
    async fn remove_peering_rules(&mut self, peering_name: &str) -> Result<()> {
        let comment = format!("peering:{}", peering_name);
        remove_rules_by_comment(&comment).await?;
        debug!("nftables: removed peering rules for '{}'", peering_name);
        Ok(())
    }

    /// Snapshot current ruleset: `nft list table inet k3rs_vpc`.
    async fn snapshot(&self) -> Result<String> {
        run_nft(&["list", "table", TABLE_NAME]).await
    }

    /// Delete the entire k3rs_vpc table (for explicit cleanup/uninstall).
    async fn cleanup(&mut self) -> Result<()> {
        run_nft(&["delete", "table", TABLE_NAME]).await?;
        info!("nftables: deleted table {}", TABLE_NAME);
        Ok(())
    }

    /// Rebuild all nftables rules from stored VPC definitions, allocations, and peerings.
    /// Called on daemon startup for crash recovery. Uses `with_context` for better error reporting.
    async fn rebuild(
        &mut self,
        vpcs: &[Vpc],
        allocations: &[StoredAllocation],
        peerings: &[VpcPeering],
    ) -> Result<()> {
        for vpc in vpcs {
            self.ensure_vpc(vpc.vpc_id, &vpc.ipv4_cidr)
                .await
                .with_context(|| {
                    format!(
                        "nftables: failed to create chains for VPC {} (id={})",
                        vpc.name, vpc.vpc_id
                    )
                })?;
        }

        for alloc in allocations {
            self.install_pod_rules(&alloc.pod_id, &alloc.guest_ipv4, alloc.vpc_id)
                .await
                .with_context(|| {
                    format!(
                        "nftables: failed to install rules for pod {} in VPC {}",
                        alloc.pod_id, alloc.vpc_name
                    )
                })?;
        }

        for peering in peerings {
            if let Err(e) = self.install_peering_rules(peering, vpcs).await {
                warn!(
                    "nftables: failed to install peering rules for '{}': {}",
                    peering.name, e
                );
            }
        }

        info!(
            "nftables: rebuilt rules for {} VPCs, {} allocations, {} peerings",
            vpcs.len(),
            allocations.len(),
            peerings.len()
        );
        Ok(())
    }
}

/// Execute `nft` with the given arguments, return stdout. Log stderr on failure.
async fn run_nft(args: &[&str]) -> Result<String> {
    debug!("nft {}", args.join(" "));
    let output = tokio::process::Command::new("nft")
        .args(args)
        .output()
        .await
        .context("failed to execute nft")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!("nft {} failed: {}", args.join(" "), stderr.trim());
        anyhow::bail!("nft command failed: {}", stderr.trim());
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Execute multiple nftables commands atomically via `nft -f -`.
/// All commands are piped to stdin as a single batch, which is both faster
/// (one process spawn instead of N) and atomic (all-or-nothing application).
async fn run_nft_batch(commands: &[&str]) -> Result<()> {
    let script = commands.join("\n");
    debug!("nft batch:\n{}", script);

    let mut child = tokio::process::Command::new("nft")
        .arg("-f")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("failed to spawn nft")?;

    use tokio::io::AsyncWriteExt;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(script.as_bytes()).await?;
        stdin.shutdown().await?;
    }

    let output = child
        .wait_with_output()
        .await
        .context("failed to wait on nft")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!("nft batch failed: {}", stderr.trim());
        anyhow::bail!("nft batch failed: {}", stderr.trim());
    }

    Ok(())
}

/// Remove all rules in `table inet k3rs_vpc` whose comment matches the given string.
/// Lists all rules as JSON, finds matching handles, then deletes them.
async fn remove_rules_by_comment(comment: &str) -> Result<()> {
    // List the entire table as JSON for programmatic parsing
    let json_str = match run_nft(&["-j", "list", "table", TABLE_NAME]).await {
        Ok(s) => s,
        Err(e) => {
            warn!("nftables: could not list table for rule removal: {}", e);
            return Ok(());
        }
    };

    let parsed: serde_json::Value =
        serde_json::from_str(&json_str).context("failed to parse nft JSON output")?;

    let items = match parsed.get("nftables").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return Ok(()),
    };

    for item in items {
        let rule = match item.get("rule") {
            Some(r) => r,
            None => continue,
        };

        // Check if this rule has our comment
        let rule_comment = rule.get("comment").and_then(|c| c.as_str()).unwrap_or("");
        if rule_comment != comment {
            continue;
        }

        // Extract chain name and handle for deletion
        let chain = match rule.get("chain").and_then(|c| c.as_str()) {
            Some(c) => c,
            None => continue,
        };
        let handle = match rule.get("handle") {
            Some(h) => h,
            None => continue,
        };

        run_nft(&[
            "delete",
            "rule",
            TABLE_NAME,
            chain,
            "handle",
            &handle.to_string(),
        ])
        .await
        .ok();
    }

    Ok(())
}
