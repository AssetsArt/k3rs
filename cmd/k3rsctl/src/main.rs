mod cli;
mod commands;
mod pm;

use clap::Parser;
use cli::Commands;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::level_filters::LevelFilter::INFO.into())
                .add_directive(tracing::level_filters::LevelFilter::ERROR.into())
                .add_directive(tracing::level_filters::LevelFilter::WARN.into()),
        )
        .init();
    let cli = cli::Cli::parse();

    // Handle PM commands before building HTTP client (PM is local-only)
    if let Commands::Pm { action } = &cli.command {
        return pm::handle(action).await;
    }

    let mut headers = reqwest::header::HeaderMap::new();
    let auth_value = format!("Bearer {}", cli.token);
    let mut auth_header = reqwest::header::HeaderValue::from_str(&auth_value)?;
    auth_header.set_sensitive(true);
    headers.insert(reqwest::header::AUTHORIZATION, auth_header);

    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .default_headers(headers)
        .build()?;

    commands::dispatch(&cli, &client).await
}
