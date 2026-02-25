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

    /// Log format: 'text' or 'json'
    #[arg(long, default_value = "text")]
    log_format: String,

    /// Enable OpenTelemetry tracing export
    #[arg(long, default_value_t = false)]
    enable_otel: bool,

    /// OpenTelemetry OTLP endpoint (gRPC)
    #[arg(long, default_value = "http://localhost:4317")]
    otel_endpoint: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Initialize logging based on format, optionally with OpenTelemetry
    if cli.enable_otel {
        init_tracing_with_otel(&cli.log_format, &cli.otel_endpoint)?;
    } else {
        init_tracing(&cli.log_format);
    }

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
    if cli.enable_otel {
        info!("  OTel:      {} (enabled)", cli.otel_endpoint);
    }

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

/// Standard tracing initialization (text or json).
fn init_tracing(log_format: &str) {
    match log_format {
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
}

/// Initialize tracing with OpenTelemetry OTLP export layer.
fn init_tracing_with_otel(log_format: &str, endpoint: &str) -> anyhow::Result<()> {
    use opentelemetry::trace::TracerProvider;
    use opentelemetry_otlp::WithExportConfig;
    use opentelemetry_sdk::trace::SdkTracerProvider;
    use tracing_opentelemetry::OpenTelemetryLayer;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .build()
        .map_err(|e| anyhow::anyhow!("Failed to create OTLP exporter: {}", e))?;

    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .build();

    let tracer = provider.tracer("k3rs-server");
    // Keep provider alive — it will be cleaned up when the process exits
    std::mem::forget(provider);

    let otel_layer = OpenTelemetryLayer::new(tracer);

    let filter = tracing_subscriber::EnvFilter::from_default_env()
        .add_directive(tracing::level_filters::LevelFilter::INFO.into());

    // Use text format when OTel is enabled (avoids complex type layering with JSON)
    let _ = log_format; // acknowledged but simplified
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .with(otel_layer)
        .init();

    info!(
        "OpenTelemetry tracing initialized — exporting to {}",
        endpoint
    );
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
