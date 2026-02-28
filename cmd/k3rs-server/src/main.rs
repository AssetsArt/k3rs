use clap::Parser;
use pkg_api::server::{ServerConfig, start_server};
use pkg_types::config::{ServerConfigFile, load_config_file};
use std::net::SocketAddr;
use tracing::info;

#[derive(Parser, Debug)]
#[command(name = "k3rs-server", about = "k3rs control plane server")]
struct Cli {
    /// Path to YAML config file
    #[cfg(debug_assertions)]
    #[arg(long, short, default_value = "example/config.yaml")]
    config: String,

    #[cfg(not(debug_assertions))]
    #[arg(long, short, default_value = pkg_constants::paths::DEFAULT_SERVER_CONFIG)]
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

    // Load config file (returns defaults if file not found)
    let file_cfg: ServerConfigFile = load_config_file(&cli.config)?;
    info!("Config file: {}", cli.config);

    // Merge: CLI args > config file > defaults
    let port = cli
        .port
        .or(file_cfg.port)
        .unwrap_or(pkg_constants::network::DEFAULT_API_PORT);
    let data_dir = cli
        .data_dir
        .or(file_cfg.data_dir)
        .unwrap_or_else(|| pkg_constants::paths::DEFAULT_SERVER_DATA_DIR.to_string());
    let token = cli
        .token
        .or(file_cfg.token)
        .unwrap_or_else(|| pkg_constants::auth::DEFAULT_JOIN_TOKEN.to_string());
    let node_name = cli
        .node_name
        .or(file_cfg.node_name)
        .unwrap_or_else(hostname);

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

/// Get the system hostname, fallback to "master".
fn hostname() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("HOST"))
        .unwrap_or_else(|_| "master".to_string())
}
