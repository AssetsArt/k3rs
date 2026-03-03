mod allocator;
mod enforcer;
mod nftables;
mod noop_enforcer;
mod protocol;
mod socket;
mod store;
mod sync;

#[cfg(all(target_os = "linux", feature = "ebpf"))]
mod ebpf_enforcer;

use std::sync::Arc;

use clap::Parser;
use pkg_types::config::{VpcConfigFile, load_config_file};
use store::VpcStore;
use tokio::sync::Mutex;
use tracing::info;

use crate::allocator::GhostAllocator;
use crate::enforcer::NetworkEnforcer;

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
}

/// Select the best available network enforcement backend.
/// Priority: eBPF (Linux + feature, if available) → nftables (Linux) → noop (any platform).
fn select_enforcer() -> Box<dyn NetworkEnforcer> {
    #[cfg(target_os = "linux")]
    {
        // Try eBPF first (only available with the ebpf feature)
        #[cfg(feature = "ebpf")]
        {
            match ebpf_enforcer::EbpfEnforcer::new() {
                Ok(e) => {
                    info!("Selected eBPF network enforcer");
                    return Box::new(e);
                }
                Err(e) => {
                    info!("eBPF not available ({}), falling back to nftables", e);
                }
            }
        }

        // Fall back to nftables
        info!("Selected nftables network enforcer");
        return Box::new(nftables::NftManager::new());
    }

    #[cfg(not(target_os = "linux"))]
    {
        info!("Selected noop network enforcer (non-Linux platform)");
        Box::new(noop_enforcer::NoopEnforcer::new())
    }
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
    let mut enforcer = select_enforcer();
    enforcer.init().await?;
    enforcer
        .rebuild(&cached_vpcs, &stored_allocations, &cached_peerings)
        .await?;
    if let Ok(snapshot) = enforcer.snapshot().await
        && let Err(e) = store.save_nft_snapshot(&snapshot).await
    {
        tracing::warn!("Failed to save enforcer snapshot: {}", e);
    }
    let enforcer: Arc<Mutex<Box<dyn NetworkEnforcer>>> = Arc::new(Mutex::new(enforcer));

    // 8. Start Unix socket listener
    let socket_handle =
        socket::start_listener(&socket_path, Arc::clone(&allocator), Arc::clone(&enforcer));

    // 9. Start VPC sync loop (every 10s)
    let sync_handle = sync::start_sync_loop(
        server_url,
        token,
        Arc::clone(&store),
        Arc::clone(&allocator),
        Arc::clone(&enforcer),
        10,
    );

    // 10. Graceful shutdown on Ctrl+C
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
