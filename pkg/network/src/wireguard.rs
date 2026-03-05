//! WireGuard mesh primitives — creates and manages the `wg-k3rs` interface
//! for cross-node pod-to-pod traffic over Ghost IPv6.
//!
//! All operations are idempotent. Shells out to `wg` and `ip` commands,
//! consistent with `bridge.rs` and `netns.rs` patterns.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

pub use pkg_constants::network::{
    GHOST_ROUTE_PREFIX, WG_DEFAULT_KEY_PATH, WG_DEFAULT_PORT, WG_INTERFACE,
};

/// A WireGuard peer as parsed from `wg show dump`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WgPeer {
    pub public_key: String,
    pub endpoint: String,
    pub allowed_ips: String,
    pub latest_handshake: u64,
    pub transfer_rx: u64,
    pub transfer_tx: u64,
}

/// Generate a WireGuard keypair. Returns `(private_key, public_key)`.
pub async fn generate_keypair() -> Result<(String, String)> {
    let output = tokio::process::Command::new("wg")
        .arg("genkey")
        .output()
        .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("wg genkey failed: {}", stderr.trim());
    }
    let private_key = String::from_utf8(output.stdout)?.trim().to_string();

    let mut child = tokio::process::Command::new("wg")
        .arg("pubkey")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        stdin.write_all(private_key.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        // Drop stdin to close it and signal EOF
    }

    let output = child.wait_with_output().await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("wg pubkey failed: {}", stderr.trim());
    }
    let public_key = String::from_utf8(output.stdout)?.trim().to_string();

    info!(
        "[wireguard] Generated keypair (pubkey: {}...)",
        &public_key[..8.min(public_key.len())]
    );
    Ok((private_key, public_key))
}

/// Create the `wg-k3rs` WireGuard interface if it doesn't exist,
/// set its private key and listen port, and bring it up. Idempotent.
pub async fn ensure_wireguard(listen_port: u16, private_key: &str) -> Result<()> {
    // Check if interface exists
    let exists = tokio::process::Command::new("ip")
        .args(["link", "show", WG_INTERFACE])
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !exists {
        // Create WireGuard interface
        let output = tokio::process::Command::new("ip")
            .args(["link", "add", WG_INTERFACE, "type", "wireguard"])
            .output()
            .await?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.contains("File exists") {
                anyhow::bail!(
                    "[wireguard] Failed to create {}: {}",
                    WG_INTERFACE,
                    stderr.trim()
                );
            }
        }
        info!("[wireguard] Created interface {}", WG_INTERFACE);
    } else {
        info!("[wireguard] Interface {} already exists", WG_INTERFACE);
    }

    // Write private key to a temp file for `wg set`
    let key_file = format!("/tmp/.wg-k3rs-privkey-{}", std::process::id());
    tokio::fs::write(&key_file, private_key).await?;

    // Set private key and listen port
    let port_str = listen_port.to_string();
    let output = tokio::process::Command::new("wg")
        .args([
            "set",
            WG_INTERFACE,
            "listen-port",
            &port_str,
            "private-key",
            &key_file,
        ])
        .output()
        .await?;

    // Clean up temp key file
    let _ = tokio::fs::remove_file(&key_file).await;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("[wireguard] wg set failed: {}", stderr.trim());
    }

    // Bring interface up
    let output = tokio::process::Command::new("ip")
        .args(["link", "set", WG_INTERFACE, "up"])
        .output()
        .await?;
    if !output.status.success() {
        warn!(
            "[wireguard] ip link set up warning: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    info!(
        "[wireguard] {} configured (port: {})",
        WG_INTERFACE, listen_port
    );
    Ok(())
}

/// Add or update a WireGuard peer.
pub async fn add_peer(public_key: &str, endpoint: &str, allowed_ips: &str) -> Result<()> {
    let output = tokio::process::Command::new("wg")
        .args([
            "set",
            WG_INTERFACE,
            "peer",
            public_key,
            "endpoint",
            endpoint,
            "allowed-ips",
            allowed_ips,
            "persistent-keepalive",
            "25",
        ])
        .output()
        .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "[wireguard] Failed to add peer {}: {}",
            &public_key[..8.min(public_key.len())],
            stderr.trim()
        );
    }
    info!(
        "[wireguard] Added peer {}... endpoint={}",
        &public_key[..8.min(public_key.len())],
        endpoint
    );
    Ok(())
}

/// Remove a WireGuard peer.
pub async fn remove_peer(public_key: &str) -> Result<()> {
    let output = tokio::process::Command::new("wg")
        .args(["set", WG_INTERFACE, "peer", public_key, "remove"])
        .output()
        .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "[wireguard] Failed to remove peer {}: {}",
            &public_key[..8.min(public_key.len())],
            stderr.trim()
        );
    }
    info!(
        "[wireguard] Removed peer {}...",
        &public_key[..8.min(public_key.len())]
    );
    Ok(())
}

/// Add the Ghost IPv6 route to the WireGuard interface. Idempotent —
/// tolerates "File exists" if the route is already present.
pub async fn ensure_ghost_route() -> Result<()> {
    let output = tokio::process::Command::new("ip")
        .args([
            "-6",
            "route",
            "add",
            GHOST_ROUTE_PREFIX,
            "dev",
            WG_INTERFACE,
        ])
        .output()
        .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.contains("File exists") {
            anyhow::bail!("[wireguard] Failed to add ghost route: {}", stderr.trim());
        }
    }
    info!(
        "[wireguard] Ghost route {} dev {} ensured",
        GHOST_ROUTE_PREFIX, WG_INTERFACE
    );
    Ok(())
}

/// List current WireGuard peers by parsing `wg show wg-k3rs dump`.
pub async fn list_peers() -> Result<Vec<WgPeer>> {
    let output = tokio::process::Command::new("wg")
        .args(["show", WG_INTERFACE, "dump"])
        .output()
        .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Interface might not exist yet
        if stderr.contains("No such device") {
            return Ok(vec![]);
        }
        anyhow::bail!("[wireguard] wg show dump failed: {}", stderr.trim());
    }

    let stdout = String::from_utf8(output.stdout)?;
    let mut peers = Vec::new();

    for (i, line) in stdout.lines().enumerate() {
        // First line is the interface itself, skip it
        if i == 0 {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() >= 6 {
            peers.push(WgPeer {
                public_key: fields[0].to_string(),
                endpoint: fields[2].to_string(),
                allowed_ips: fields[3].to_string(),
                latest_handshake: fields[4].parse().unwrap_or(0),
                transfer_rx: fields[5].parse().unwrap_or(0),
                transfer_tx: fields.get(6).and_then(|s| s.parse().ok()).unwrap_or(0),
            });
        }
    }

    Ok(peers)
}

/// Tear down the WireGuard interface entirely.
pub async fn teardown_wireguard() -> Result<()> {
    let output = tokio::process::Command::new("ip")
        .args(["link", "delete", WG_INTERFACE])
        .output()
        .await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.contains("Cannot find device") {
            anyhow::bail!(
                "[wireguard] Failed to delete {}: {}",
                WG_INTERFACE,
                stderr.trim()
            );
        }
    }
    info!("[wireguard] Interface {} removed", WG_INTERFACE);
    Ok(())
}
