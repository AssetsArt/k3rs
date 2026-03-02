//! Async Unix socket listener with NDJSON protocol for the VPC daemon.

use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

use crate::protocol::{VpcInfo, VpcRequest, VpcResponse};
use crate::store::VpcStore;

/// Start the Unix socket listener. Returns a `JoinHandle` for the accept loop.
pub fn start_listener(socket_path: &str, store: Arc<VpcStore>) -> JoinHandle<()> {
    // Remove stale socket file if it exists
    let _ = std::fs::remove_file(socket_path);

    // Ensure parent directory exists
    if let Some(parent) = std::path::Path::new(socket_path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let listener = UnixListener::bind(socket_path).expect("failed to bind VPC socket");
    info!("VPC socket listener started on {}", socket_path);

    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _addr)) => {
                    let store = Arc::clone(&store);
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(stream, &store).await {
                            warn!("VPC socket connection error: {}", e);
                        }
                    });
                }
                Err(e) => {
                    error!("VPC socket accept error: {}", e);
                }
            }
        }
    })
}

async fn handle_connection(
    stream: tokio::net::UnixStream,
    store: &VpcStore,
) -> anyhow::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    while let Some(line) = lines.next_line().await? {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        let response = match serde_json::from_str::<VpcRequest>(&line) {
            Ok(req) => dispatch(req, store).await,
            Err(e) => VpcResponse::Error {
                code: "parse_error".to_string(),
                message: format!("Invalid request: {}", e),
            },
        };

        let mut out = serde_json::to_string(&response)?;
        out.push('\n');
        writer.write_all(out.as_bytes()).await?;
    }

    Ok(())
}

async fn dispatch(req: VpcRequest, store: &VpcStore) -> VpcResponse {
    match req {
        VpcRequest::Ping => VpcResponse::Pong,
        VpcRequest::ListVpcs => match store.load_vpcs().await {
            Ok(vpcs) => VpcResponse::VpcList {
                vpcs: vpcs
                    .into_iter()
                    .map(|v| VpcInfo {
                        name: v.name,
                        vpc_id: v.vpc_id,
                        ipv4_cidr: v.ipv4_cidr,
                        status: v.status,
                    })
                    .collect(),
            },
            Err(e) => VpcResponse::Error {
                code: "store_error".to_string(),
                message: format!("Failed to load VPCs: {}", e),
            },
        },
        _ => VpcResponse::Error {
            code: "not_implemented".to_string(),
            message: "This command is not yet implemented".to_string(),
        },
    }
}
