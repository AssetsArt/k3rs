//! nftables rule management engine for VPC isolation enforcement.
//!
//! Manages `table inet k3rs_vpc` with per-VPC ingress/egress chains,
//! per-pod forwarding rules, anti-spoofing, and TAP interface rules.
//! Uses `nft` CLI for rule manipulation (no library dependency).

use std::collections::HashSet;

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use crate::store::StoredAllocation;
use pkg_types::vpc::Vpc;

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

    /// Create `table inet k3rs_vpc` with the base `forward` chain and `input_validation` chain.
    /// Idempotent (uses `add` not `create`).
    pub async fn init_table(&self) -> Result<()> {
        // Create the table (idempotent with `add`)
        run_nft(&["add", "table", TABLE_NAME]).await?;

        // Base forward chain: filter hook, priority 0, policy accept.
        // policy accept so non-VPC traffic is unaffected; per-VPC chains enforce drop.
        run_nft(&[
            "add",
            "chain",
            TABLE_NAME,
            "forward",
            "{ type filter hook forward priority 0; policy accept; }",
        ])
        .await?;

        // Anti-spoofing chain: input hook, priority -1 (before conntrack), policy accept.
        run_nft(&[
            "add",
            "chain",
            TABLE_NAME,
            "input_validation",
            "{ type filter hook input priority -1; policy accept; }",
        ])
        .await?;

        info!("nftables: initialized table {}", TABLE_NAME);
        Ok(())
    }

    /// Create per-VPC ingress and egress chains with intra-VPC accept + default drop.
    /// Idempotent — tracks in `active_vpc_chains`.
    pub async fn ensure_vpc_chains(&mut self, vpc_id: u16, cidr: &str) -> Result<()> {
        if self.active_vpc_chains.contains(&vpc_id) {
            return Ok(());
        }

        let ingress = format!("vpc_{}_ingress", vpc_id);
        let egress = format!("vpc_{}_egress", vpc_id);

        // Create chains (regular chains, not base chains — no hook)
        run_nft(&["add", "chain", TABLE_NAME, &ingress]).await?;
        run_nft(&["add", "chain", TABLE_NAME, &egress]).await?;

        // Intra-VPC accept rules
        let intra_rule = format!("ip saddr {} ip daddr {} accept", cidr, cidr);
        run_nft(&["add", "rule", TABLE_NAME, &ingress, &intra_rule]).await?;
        run_nft(&["add", "rule", TABLE_NAME, &egress, &intra_rule]).await?;

        // Default drop at end of chain
        run_nft(&["add", "rule", TABLE_NAME, &ingress, "drop"]).await?;
        run_nft(&["add", "rule", TABLE_NAME, &egress, "drop"]).await?;

        self.active_vpc_chains.insert(vpc_id);
        info!("nftables: created VPC chains for vpc_id={} cidr={}", vpc_id, cidr);
        Ok(())
    }

    /// Delete VPC chains when a VPC is removed.
    pub async fn remove_vpc_chains(&mut self, vpc_id: u16) -> Result<()> {
        if !self.active_vpc_chains.remove(&vpc_id) {
            return Ok(());
        }

        let ingress = format!("vpc_{}_ingress", vpc_id);
        let egress = format!("vpc_{}_egress", vpc_id);

        // Flush then delete (flush removes rules so delete succeeds)
        run_nft(&["flush", "chain", TABLE_NAME, &ingress]).await.ok();
        run_nft(&["delete", "chain", TABLE_NAME, &ingress]).await.ok();
        run_nft(&["flush", "chain", TABLE_NAME, &egress]).await.ok();
        run_nft(&["delete", "chain", TABLE_NAME, &egress]).await.ok();

        info!("nftables: removed VPC chains for vpc_id={}", vpc_id);
        Ok(())
    }

    /// Install forwarding rules and anti-spoofing for a pod.
    /// Uses `comment "pod:<pod_id>"` for targeted removal.
    pub async fn install_pod_rules(
        &self,
        pod_id: &str,
        guest_ipv4: &str,
        vpc_id: u16,
    ) -> Result<()> {
        let comment = format!("pod:{}", pod_id);
        let egress_chain = format!("vpc_{}_egress", vpc_id);
        let ingress_chain = format!("vpc_{}_ingress", vpc_id);

        // Forward chain: jump to VPC egress for traffic FROM this pod
        run_nft(&[
            "add",
            "rule",
            TABLE_NAME,
            "forward",
            &format!(
                "ip saddr {} jump {} comment \"{}\"",
                guest_ipv4, egress_chain, comment
            ),
        ])
        .await?;

        // Forward chain: jump to VPC ingress for traffic TO this pod
        run_nft(&[
            "add",
            "rule",
            TABLE_NAME,
            "forward",
            &format!(
                "ip daddr {} jump {} comment \"{}\"",
                guest_ipv4, ingress_chain, comment
            ),
        ])
        .await?;

        // Anti-spoofing: drop packets from TAP interfaces claiming this pod's IP
        // but arriving on the wrong interface. Only for TAP interfaces.
        run_nft(&[
            "add",
            "rule",
            TABLE_NAME,
            "input_validation",
            &format!(
                "iifname \"tap-*\" ip saddr != {} drop comment \"{}\"",
                guest_ipv4, comment
            ),
        ])
        .await?;

        debug!("nftables: installed pod rules for pod={} ipv4={} vpc_id={}", pod_id, guest_ipv4, vpc_id);
        Ok(())
    }

    /// Remove all rules with comment `pod:<pod_id>` by listing + deleting by handle.
    pub async fn remove_pod_rules(&self, pod_id: &str) -> Result<()> {
        let comment = format!("pod:{}", pod_id);
        remove_rules_by_comment(&comment).await?;
        debug!("nftables: removed pod rules for pod={}", pod_id);
        Ok(())
    }

    /// Install TAP-specific rules matching iifname/oifname on the TAP device.
    /// Uses `comment "tap:<tap_name>"`.
    pub async fn install_tap_rules(
        &self,
        tap_name: &str,
        guest_ipv4: &str,
        vpc_id: u16,
    ) -> Result<()> {
        let comment = format!("tap:{}", tap_name);
        let egress_chain = format!("vpc_{}_egress", vpc_id);
        let ingress_chain = format!("vpc_{}_ingress", vpc_id);

        // Traffic leaving the TAP (from VM) → VPC egress chain
        run_nft(&[
            "add",
            "rule",
            TABLE_NAME,
            "forward",
            &format!(
                "iifname \"{}\" ip saddr {} jump {} comment \"{}\"",
                tap_name, guest_ipv4, egress_chain, comment
            ),
        ])
        .await?;

        // Traffic entering the TAP (to VM) → VPC ingress chain
        run_nft(&[
            "add",
            "rule",
            TABLE_NAME,
            "forward",
            &format!(
                "oifname \"{}\" ip daddr {} jump {} comment \"{}\"",
                tap_name, guest_ipv4, ingress_chain, comment
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
    pub async fn remove_tap_rules(&self, tap_name: &str) -> Result<()> {
        let comment = format!("tap:{}", tap_name);
        remove_rules_by_comment(&comment).await?;
        debug!("nftables: removed TAP rules for tap={}", tap_name);
        Ok(())
    }

    /// Snapshot current ruleset: `nft list table inet k3rs_vpc`.
    pub async fn snapshot(&self) -> Result<String> {
        run_nft(&["list", "table", TABLE_NAME]).await
    }

    /// Rebuild all nftables rules from stored VPC definitions and allocations.
    /// Called on daemon startup for crash recovery.
    pub async fn rebuild_from_allocations(
        &mut self,
        vpcs: &[Vpc],
        allocations: &[StoredAllocation],
    ) -> Result<()> {
        // Create VPC chains for all known VPCs
        for vpc in vpcs {
            self.ensure_vpc_chains(vpc.vpc_id, &vpc.ipv4_cidr)
                .await
                .with_context(|| {
                    format!(
                        "nftables: failed to create chains for VPC {} (id={})",
                        vpc.name, vpc.vpc_id
                    )
                })?;
        }

        // Install rules for all existing allocations
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

        info!(
            "nftables: rebuilt rules for {} VPCs, {} allocations",
            vpcs.len(),
            allocations.len()
        );
        Ok(())
    }

    /// Delete the entire k3rs_vpc table (for explicit cleanup/uninstall).
    pub async fn cleanup(&self) -> Result<()> {
        run_nft(&["delete", "table", TABLE_NAME]).await?;
        info!("nftables: deleted table {}", TABLE_NAME);
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

/// Remove all rules in `table inet k3rs_vpc` whose comment matches the given string.
/// Lists all rules as JSON, finds matching handles, then deletes them.
async fn remove_rules_by_comment(comment: &str) -> Result<()> {
    // List the entire table as JSON for programmatic parsing
    let output = run_nft(&["-j", "list", "table", TABLE_NAME]).await;
    let json_str = match output {
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
