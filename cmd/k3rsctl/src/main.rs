use clap::{Parser, Subcommand};
use pkg_types::node::{ClusterInfo, Node};
use tracing::info;

#[derive(Parser)]
#[command(name = "k3rsctl", about = "CLI tool for k3rs cluster management")]
struct Cli {
    /// Server API endpoint
    #[arg(long, default_value = "http://127.0.0.1:6443")]
    server: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Show cluster information
    Cluster {
        #[command(subcommand)]
        action: ClusterAction,
    },
    /// Manage nodes
    Node {
        #[command(subcommand)]
        action: NodeAction,
    },
}

#[derive(Subcommand)]
enum ClusterAction {
    /// Display cluster info
    Info,
}

#[derive(Subcommand)]
enum NodeAction {
    /// List all registered nodes
    List,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()?;

    match &cli.command {
        Commands::Cluster { action } => match action {
            ClusterAction::Info => {
                info!("Querying cluster info from {}", cli.server);
                let url = format!("{}/api/v1/cluster/info", cli.server.trim_end_matches('/'));
                let resp = client.get(&url).send().await?;

                if !resp.status().is_success() {
                    eprintln!("Error: server returned {}", resp.status());
                    std::process::exit(1);
                }

                let info: ClusterInfo = resp.json().await?;
                println!("Cluster Endpoint:  {}", info.endpoint);
                println!("Version:           {}", info.version);
                println!("State Store:       {}", info.state_store);
                println!("Nodes:             {}", info.node_count);
            }
        },
        Commands::Node { action } => match action {
            NodeAction::List => {
                info!("Querying node list from {}", cli.server);
                let url = format!("{}/api/v1/nodes", cli.server.trim_end_matches('/'));
                let resp = client.get(&url).send().await?;

                if !resp.status().is_success() {
                    eprintln!("Error: server returned {}", resp.status());
                    std::process::exit(1);
                }

                let nodes: Vec<Node> = resp.json().await?;

                println!(
                    "{:<38} {:<16} {:<10} {}",
                    "ID", "NAME", "STATUS", "REGISTERED"
                );
                for node in &nodes {
                    println!(
                        "{:<38} {:<16} {:<10} {}",
                        node.id,
                        node.name,
                        node.status,
                        node.registered_at.format("%Y-%m-%d %H:%M:%S")
                    );
                }

                if nodes.is_empty() {
                    println!("(no nodes registered)");
                }
            }
        },
    }

    Ok(())
}
