use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "k3rs-agent", about = "k3rs node agent (data plane)")]
pub struct Cli {
    /// Path to YAML config file
    #[arg(long, short, default_value_t = format!("{}/agent-config.yaml", pkg_constants::paths::CONFIG_DIR))]
    pub config: String,

    /// Server API endpoint
    #[arg(long)]
    pub server: Option<String>,

    /// Join token for registration
    #[arg(long)]
    pub token: Option<String>,

    /// Node name
    #[arg(long)]
    pub node_name: Option<String>,

    /// Local port for the tunnel proxy
    #[arg(long)]
    pub proxy_port: Option<u16>,

    /// Local port for the service proxy
    #[arg(long)]
    pub service_proxy_port: Option<u16>,

    /// Local port for the embedded DNS server
    #[arg(long)]
    pub dns_port: Option<u16>,

    /// Log format: 'text' or 'json'
    #[arg(long, default_value = "text")]
    pub log_format: String,

    /// Path to the local data directory (AgentStore / SlateDB location)
    #[arg(long, default_value_t = pkg_constants::paths::DATA_DIR.to_string())]
    pub data_dir: String,

    /// Path to the VPC daemon Unix socket
    #[arg(long, default_value_t = pkg_constants::paths::VPC_SOCKET.to_string())]
    pub vpc_socket: String,
}
