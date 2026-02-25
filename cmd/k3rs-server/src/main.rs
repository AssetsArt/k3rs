use clap::Parser;
use pkg_api::server::{ServerConfig, start_server};
use pkg_types::config::{ServerConfigFile, load_config_file};
use std::net::SocketAddr;
use std::path::Path;
use tracing::{error, info};

const SERVER_LOCK: &str = "/tmp/k3rs-server.lock";
const AGENT_LOCK: &str = "/tmp/k3rs-agent.lock";

#[derive(Parser, Debug)]
#[command(name = "k3rs-server", about = "k3rs control plane server")]
struct Cli {
    /// Path to YAML config file
    #[arg(long, short, default_value = "/etc/k3rs/config.yaml")]
    config: String,

    /// Port to listen on
    #[arg(long)]
    port: Option<u16>,

    /// Directory for SlateDB state storage
    #[arg(long)]
    data_dir: Option<String>,

    /// Join token for agent registration
    #[arg(long)]
    token: Option<String>,

    /// Name for this master/control-plane node
    #[arg(long)]
    node_name: Option<String>,

    /// Allow running alongside k3rs-agent on the same machine (dev only)
    #[arg(long, default_value_t = false)]
    allow_colocate: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    // Colocation guard: prevent server + agent on the same machine
    if !cli.allow_colocate && is_process_alive(AGENT_LOCK) {
        error!("❌ k3rs-agent is already running on this machine.");
        error!("   Running server and agent on the same node is not supported.");
        error!("   Use --allow-colocate to override (dev only).");
        std::process::exit(1);
    }
    write_lock_file(SERVER_LOCK)?;

    // Load config file (returns defaults if file not found)
    let file_cfg: ServerConfigFile = load_config_file(&cli.config)?;
    info!("Config file: {}", cli.config);

    // Merge: CLI args > config file > defaults
    let port = cli.port.or(file_cfg.port).unwrap_or(6443);
    let data_dir = cli
        .data_dir
        .or(file_cfg.data_dir)
        .unwrap_or_else(|| "/tmp/k3rs-data".to_string());
    let token = cli
        .token
        .or(file_cfg.token)
        .unwrap_or_else(|| "demo-token-123".to_string());
    let node_name = cli
        .node_name
        .or(file_cfg.node_name)
        .unwrap_or_else(|| hostname());

    info!("Starting k3rs-server");
    info!("  Node:      {}", node_name);
    info!("  Port:      {}", port);
    info!("  Data dir:  {}", data_dir);
    info!("  Token:     {}***", &token[..token.len().min(4)]);

    let config = ServerConfig {
        addr: SocketAddr::from(([0, 0, 0, 0], port)),
        data_dir,
        join_token: token,
        node_name: node_name.clone(),
        server_id: node_name,
    };

    start_server(config).await?;

    // Clean up lock file on graceful exit
    let _ = std::fs::remove_file(SERVER_LOCK);

    Ok(())
}

/// Write a lock file containing the current PID.
fn write_lock_file(path: &str) -> anyhow::Result<()> {
    std::fs::write(path, std::process::id().to_string())?;
    Ok(())
}

/// Check if a lock file exists and the PID inside is still alive.
fn is_process_alive(lock_path: &str) -> bool {
    let path = Path::new(lock_path);
    if !path.exists() {
        return false;
    }
    match std::fs::read_to_string(path) {
        Ok(pid_str) => {
            if let Ok(pid) = pid_str.trim().parse::<i32>() {
                // kill(pid, 0) checks if process exists without sending a signal
                unsafe { libc::kill(pid, 0) == 0 }
            } else {
                // Corrupt lock file, remove it
                let _ = std::fs::remove_file(path);
                false
            }
        }
        Err(_) => false,
    }
}

/// Get the system hostname, fallback to "master".
fn hostname() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("HOST"))
        .unwrap_or_else(|_| "master".to_string())
}
