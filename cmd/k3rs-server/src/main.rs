use clap::Parser;
use pkg_api::server::{ServerConfig, start_server};
use pkg_types::config::{ServerConfigFile, load_config_file};
use std::net::SocketAddr;
use tracing::info;

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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

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

    info!("Starting k3rs-server");
    info!("  Port:      {}", port);
    info!("  Data dir:  {}", data_dir);
    info!("  Token:     {}***", &token[..token.len().min(4)]);

    let config = ServerConfig {
        addr: SocketAddr::from(([0, 0, 0, 0], port)),
        data_dir,
        join_token: token,
    };

    start_server(config).await?;

    Ok(())
}
