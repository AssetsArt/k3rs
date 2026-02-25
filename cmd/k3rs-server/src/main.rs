use clap::Parser;
use pkg_api::server::{ServerConfig, start_server};
use std::net::SocketAddr;
use tracing::info;

#[derive(Parser, Debug)]
#[command(name = "k3rs-server", about = "k3rs control plane server")]
struct Cli {
    /// Port to listen on
    #[arg(long, default_value = "6443")]
    port: u16,

    /// Directory for SlateDB state storage
    #[arg(long, default_value = "/tmp/k3rs-data")]
    data_dir: String,

    /// Join token for agent registration
    #[arg(long, default_value = "demo-token-123")]
    token: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    info!("Starting k3rs-server");
    info!("  Port:      {}", cli.port);
    info!("  Data dir:  {}", cli.data_dir);
    info!("  Token:     {}***", &cli.token[..cli.token.len().min(4)]);

    let config = ServerConfig {
        addr: SocketAddr::from(([0, 0, 0, 0], cli.port)),
        data_dir: cli.data_dir,
        join_token: cli.token,
    };

    start_server(config).await?;

    Ok(())
}
