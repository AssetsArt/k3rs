mod allocator;
mod socket;
mod sync;
mod wireguard;

#[cfg(all(target_os = "linux", feature = "ebpf"))]
mod ebpf_enforcer;

use std::sync::Arc;

use clap::Parser;
use k3rs_vpc::enforcer::NetworkEnforcer;
use k3rs_vpc::store::VpcStore;
use pkg_types::config::{VpcConfigFile, load_config_file};
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::allocator::GhostAllocator;

#[derive(Parser, Debug)]
#[command(
    name = "k3rs-vpc",
    about = "k3rs VPC daemon — manages VPC state per node"
)]
struct Cli {
    /// Path to YAML config file
    #[arg(long, short, default_value_t = format!("{}/vpc-config.yaml", pkg_constants::paths::CONFIG_DIR))]
    config: String,

    /// Server API endpoint
    #[arg(long)]
    server_url: Option<String>,

    /// Join token
    #[arg(long)]
    token: Option<String>,

    /// Path to the local data directory
    #[arg(long)]
    data_dir: Option<String>,

    /// Unix socket path
    #[arg(long)]
    socket: Option<String>,

    /// Log format: 'text' or 'json'
    #[arg(long, default_value = "text")]
    log_format: String,

    /// WireGuard listen port
    #[arg(long)]
    wg_listen_port: Option<u16>,

    /// Path to store WireGuard keys
    #[arg(long)]
    wg_key_path: Option<String>,

    /// Node IPv4 address for NAT64 (auto-detected from default route if not set)
    #[arg(long)]
    node_ipv4: Option<String>,

    /// Physical network interface for NAT64 (auto-detected from default route if not set)
    #[arg(long)]
    phys_iface: Option<String>,
}

/// Auto-detect the default route interface and gateway IPv4 by parsing `ip route show default`.
/// Returns `(interface, ipv4)` or `None` if detection fails.
fn detect_default_route() -> Option<(String, String)> {
    let output = std::process::Command::new("ip")
        .args(["route", "show", "default"])
        .output()
        .ok()?;
    let line = String::from_utf8_lossy(&output.stdout);
    // Format: "default via <gateway> dev <iface> ..."
    // We need the iface name and the node's own IPv4 on that iface.
    let parts: Vec<&str> = line.split_whitespace().collect();
    let dev_idx = parts.iter().position(|&p| p == "dev")?;
    let iface = parts.get(dev_idx + 1)?.to_string();

    // Get the node's IPv4 on this interface via `ip -4 addr show dev <iface>`
    let addr_output = std::process::Command::new("ip")
        .args(["-4", "addr", "show", "dev", &iface])
        .output()
        .ok()?;
    let addr_text = String::from_utf8_lossy(&addr_output.stdout);
    // Look for "inet <ip>/<prefix>" line
    for line in addr_text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("inet ") {
            let ip_cidr = trimmed.split_whitespace().nth(1)?;
            let ip = ip_cidr.split('/').next()?.to_string();
            return Some((iface, ip));
        }
    }
    None
}

/// Select the best available network enforcement backend.
/// Priority: eBPF (Linux + feature) → noop (fallback / non-Linux).
fn select_enforcer(
    #[cfg_attr(
        not(all(target_os = "linux", feature = "ebpf")),
        allow(unused_variables)
    )]
    platform_prefix: u32,
    #[cfg_attr(
        not(all(target_os = "linux", feature = "ebpf")),
        allow(unused_variables)
    )]
    cluster_id: u32,
) -> Box<dyn NetworkEnforcer> {
    #[cfg(all(target_os = "linux", feature = "ebpf"))]
    {
        match ebpf_enforcer::EbpfEnforcer::new(platform_prefix, cluster_id) {
            Ok(e) => {
                info!("Selected eBPF network enforcer");
                return Box::new(e);
            }
            Err(e) => {
                info!("eBPF not available ({}), falling back to noop", e);
            }
        }
    }

    info!("Selected noop network enforcer");
    Box::new(k3rs_vpc::noop_enforcer::NoopEnforcer::new())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // 1. Init tracing
    match cli.log_format.as_str() {
        "json" => {
            tracing_subscriber::fmt()
                .json()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::from_default_env()
                        .add_directive(tracing::level_filters::LevelFilter::INFO.into()),
                )
                .init();
        }
        _ => {
            tracing_subscriber::fmt::init();
        }
    }

    // 2. Load config file, merge CLI > config > defaults
    let file_cfg: VpcConfigFile = load_config_file(&cli.config)?;
    info!("Config file: {}", cli.config);

    let server_url = cli
        .server_url
        .or(file_cfg.server_url)
        .unwrap_or_else(|| pkg_constants::network::DEFAULT_API_ADDR.to_string());
    let token = cli
        .token
        .or(file_cfg.token)
        .unwrap_or_else(|| pkg_constants::auth::DEFAULT_JOIN_TOKEN.to_string());
    let data_dir = cli
        .data_dir
        .or(file_cfg.data_dir)
        .unwrap_or_else(|| pkg_constants::paths::DATA_DIR.to_string());
    let socket_path = cli
        .socket
        .or(file_cfg.socket)
        .unwrap_or_else(|| "/run/k3rs-vpc.sock".to_string());

    info!(
        "server_url={}, data_dir={}, socket={}",
        server_url, data_dir, socket_path
    );

    // 3. Open VpcStore
    let store = VpcStore::open(&data_dir).await?;
    let store = Arc::new(store);

    // 4. Load cached VPCs and allocations from store
    let cached_vpcs = store.load_vpcs().await?;
    let cached_peerings = store.load_peerings().await?;
    let stored_allocations = store.load_all_allocations().await?;
    info!(
        "Loaded {} cached VPCs, {} cached peerings, {} stored allocations from store",
        cached_vpcs.len(),
        cached_peerings.len(),
        stored_allocations.len()
    );

    // 5. Load meta for cluster_id and platform_prefix
    let meta = store.load_meta().await?;
    let platform_prefix = meta
        .as_ref()
        .map(|m| m.platform_prefix)
        .unwrap_or(pkg_vpc::constants::PLATFORM_PREFIX);
    let cluster_id = meta.as_ref().and_then(|m| m.cluster_id).unwrap_or(0);

    // 6. Create GhostAllocator and rebuild pools
    let mut allocator = GhostAllocator::new(platform_prefix, cluster_id, Arc::clone(&store));
    allocator.rebuild_pools(&cached_vpcs, &stored_allocations);
    let allocator = Arc::new(Mutex::new(allocator));

    // 7. Initialize network enforcer and rebuild rules from stored state
    let mut enforcer = select_enforcer(platform_prefix, cluster_id);
    enforcer.init().await?;
    enforcer
        .rebuild(&cached_vpcs, &stored_allocations, &cached_peerings)
        .await?;
    if let Ok(snapshot) = enforcer.snapshot().await
        && let Err(e) = store.save_enforcer_snapshot(&snapshot).await
    {
        tracing::warn!("Failed to save enforcer snapshot: {}", e);
    }
    // 7b. Install NAT64 translation (non-fatal on failure)
    let nat64_detected = detect_default_route();
    let node_ipv4 = cli
        .node_ipv4
        .or(file_cfg.node_ipv4)
        .or_else(|| nat64_detected.as_ref().map(|(_, ip)| ip.clone()));
    let phys_iface = cli
        .phys_iface
        .or(file_cfg.phys_iface)
        .or_else(|| nat64_detected.as_ref().map(|(iface, _)| iface.clone()));

    if let (Some(ipv4), Some(phys)) = (&node_ipv4, &phys_iface) {
        match enforcer.install_nat64(ipv4, "k3rs0", phys).await {
            Ok(()) => info!("NAT64 installed (node_ipv4={}, phys_iface={})", ipv4, phys),
            Err(e) => warn!(
                "NAT64 install failed: {} (pods cannot reach external IPv4)",
                e
            ),
        }
    } else {
        warn!(
            "NAT64 skipped: node_ipv4={:?}, phys_iface={:?} (use --node-ipv4 / --phys-iface or auto-detect requires a default route)",
            node_ipv4, phys_iface
        );
    }

    let enforcer: Arc<Mutex<Box<dyn NetworkEnforcer>>> = Arc::new(Mutex::new(enforcer));

    // 8. Initialize WireGuard mesh manager (Linux only, non-fatal on failure)
    let wg_listen_port = cli
        .wg_listen_port
        .or(file_cfg.wg_listen_port)
        .unwrap_or(pkg_network::wireguard::WG_DEFAULT_PORT);
    let wg_key_path = cli
        .wg_key_path
        .or(file_cfg.wg_key_path)
        .unwrap_or_else(|| pkg_network::wireguard::WG_DEFAULT_KEY_PATH.to_string());

    let wg_manager = match wireguard::WireGuardManager::init(wg_listen_port, &wg_key_path).await {
        Ok(mgr) => {
            info!(
                "WireGuard mesh initialized (pubkey: {}..., port: {})",
                &mgr.public_key()[..8.min(mgr.public_key().len())],
                mgr.listen_port()
            );
            Some(Arc::new(mgr))
        }
        Err(e) => {
            tracing::warn!(
                "WireGuard mesh not available ({}), cross-node traffic disabled",
                e
            );
            None
        }
    };

    // 9. Start Unix socket listener
    let socket_handle = socket::start_listener(
        &socket_path,
        Arc::clone(&allocator),
        Arc::clone(&enforcer),
        wg_manager.clone(),
    );

    // 10. Start VPC sync loop (every 10s)
    let sync_handle = sync::start_sync_loop(
        server_url,
        token,
        Arc::clone(&store),
        Arc::clone(&allocator),
        Arc::clone(&enforcer),
        wg_manager.clone(),
        10,
    );

    // 11. Graceful shutdown on Ctrl+C
    info!("k3rs-vpc daemon running. Press Ctrl+C to stop.");
    tokio::signal::ctrl_c().await?;
    info!("Shutting down k3rs-vpc daemon...");

    // Abort background tasks
    socket_handle.abort();
    sync_handle.abort();

    // Clean up socket file
    let _ = std::fs::remove_file(&socket_path);

    // Note: enforcement rules are NOT cleaned up on graceful shutdown.
    // Rules persist for zero-downtime restarts. Use --cleanup flag for uninstall.

    // Close the store (flush WAL)
    drop(allocator);
    drop(enforcer);
    Arc::try_unwrap(store)
        .map_err(|_| anyhow::anyhow!("VpcStore still has outstanding references"))?
        .close()
        .await?;

    info!("k3rs-vpc daemon stopped.");
    Ok(())
}
