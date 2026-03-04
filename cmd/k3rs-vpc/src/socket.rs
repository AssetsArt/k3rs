//! Async Unix socket listener with NDJSON protocol for the VPC daemon.

use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

use crate::allocator::GhostAllocator;
use k3rs_vpc::enforcer::NetworkEnforcer;
use k3rs_vpc::protocol::{VpcInfo, VpcRequest, VpcResponse};

/// Start the Unix socket listener. Returns a `JoinHandle` for the accept loop.
pub fn start_listener(
    socket_path: &str,
    allocator: Arc<Mutex<GhostAllocator>>,
    enforcer: Arc<Mutex<Box<dyn NetworkEnforcer>>>,
) -> JoinHandle<()> {
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
                    let allocator = Arc::clone(&allocator);
                    let enforcer = Arc::clone(&enforcer);
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(stream, allocator, enforcer).await {
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
    allocator: Arc<Mutex<GhostAllocator>>,
    enforcer: Arc<Mutex<Box<dyn NetworkEnforcer>>>,
) -> anyhow::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    while let Some(line) = lines.next_line().await? {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        let response = match serde_json::from_str::<VpcRequest>(&line) {
            Ok(req) => dispatch(req, &allocator, &enforcer).await,
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

async fn dispatch(
    req: VpcRequest,
    allocator: &Arc<Mutex<GhostAllocator>>,
    enforcer: &Arc<Mutex<Box<dyn NetworkEnforcer>>>,
) -> VpcResponse {
    match req {
        VpcRequest::Ping => VpcResponse::Pong,
        VpcRequest::ListVpcs => {
            let alloc = allocator.lock().await;
            match alloc.store().load_vpcs().await {
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
            }
        }
        VpcRequest::Allocate { pod_id, vpc_name } => {
            let mut alloc = allocator.lock().await;
            match alloc.allocate(&pod_id, &vpc_name).await {
                Ok(result) => VpcResponse::Allocated {
                    guest_ipv4: result.guest_ipv4.to_string(),
                    ghost_ipv6: result.ghost_ipv6.to_string(),
                    vpc_id: result.vpc_id,
                },
                Err(e) => VpcResponse::Error {
                    code: "allocate_error".to_string(),
                    message: e.to_string(),
                },
            }
        }
        VpcRequest::Release { pod_id, vpc_name } => {
            let mut alloc = allocator.lock().await;
            match alloc.release(&pod_id, &vpc_name).await {
                Ok(()) => VpcResponse::Released,
                Err(e) => VpcResponse::Error {
                    code: "release_error".to_string(),
                    message: e.to_string(),
                },
            }
        }
        VpcRequest::GetRoutes { vpc_id } => {
            let alloc = allocator.lock().await;
            let entries = alloc
                .get_routes(vpc_id)
                .into_iter()
                .map(
                    |(_pod_id, guest_ipv4, ghost_ipv6)| k3rs_vpc::protocol::RouteEntry {
                        destination: guest_ipv4,
                        next_hop: ghost_ipv6,
                    },
                )
                .collect();
            VpcResponse::Routes { entries }
        }
        VpcRequest::Query { pod_id } => {
            let alloc = allocator.lock().await;
            match alloc.query(&pod_id) {
                Some(result) => VpcResponse::QueryResult {
                    guest_ipv4: result.guest_ipv4.to_string(),
                    ghost_ipv6: result.ghost_ipv6.to_string(),
                    vpc_id: result.vpc_id,
                    vpc_name: result.vpc_name,
                },
                None => VpcResponse::Error {
                    code: "not_found".to_string(),
                    message: format!("No allocation for pod '{}'", pod_id),
                },
            }
        }
        VpcRequest::AttachNetkit {
            nk_name,
            guest_ipv4,
            ghost_ipv6,
            vpc_id,
            vpc_cidr,
            container_pid,
        } => {
            let mut enf = enforcer.lock().await;
            match enf
                .install_netkit_rules(
                    &nk_name,
                    &guest_ipv4,
                    &ghost_ipv6,
                    vpc_id,
                    &vpc_cidr,
                    container_pid,
                )
                .await
            {
                Ok(()) => VpcResponse::Ok,
                Err(e) => VpcResponse::Error {
                    code: "attach_netkit_error".to_string(),
                    message: format!("Failed to attach TC to netkit {}: {}", nk_name, e),
                },
            }
        }
        VpcRequest::DetachNetkit { nk_name } => {
            let mut enf = enforcer.lock().await;
            match enf.remove_netkit_rules(&nk_name).await {
                Ok(()) => VpcResponse::Ok,
                Err(e) => VpcResponse::Error {
                    code: "detach_netkit_error".to_string(),
                    message: format!("Failed to detach TC from netkit {}: {}", nk_name, e),
                },
            }
        }
        VpcRequest::AttachTap {
            tap_name,
            guest_ipv4,
            ghost_ipv6,
            vpc_id,
            vpc_cidr,
        } => {
            let mut enf = enforcer.lock().await;
            match enf
                .install_tap_rules(&tap_name, &guest_ipv4, &ghost_ipv6, vpc_id, &vpc_cidr)
                .await
            {
                Ok(()) => VpcResponse::Ok,
                Err(e) => VpcResponse::Error {
                    code: "attach_tap_error".to_string(),
                    message: format!("Failed to attach TC to TAP {}: {}", tap_name, e),
                },
            }
        }
        VpcRequest::DetachTap { tap_name } => {
            let mut enf = enforcer.lock().await;
            match enf.remove_tap_rules(&tap_name).await {
                Ok(()) => VpcResponse::Ok,
                Err(e) => VpcResponse::Error {
                    code: "detach_tap_error".to_string(),
                    message: format!("Failed to detach TC from TAP {}: {}", tap_name, e),
                },
            }
        }
        VpcRequest::CheckReachability { src_vpc, dst_vpc } => {
            // Same VPC is always reachable
            if src_vpc == dst_vpc {
                return VpcResponse::Reachable { reachable: true };
            }

            let alloc = allocator.lock().await;
            match alloc.store().load_peerings().await {
                Ok(peerings) => {
                    let reachable = peerings.iter().any(|p| {
                        if p.status != pkg_types::vpc::PeeringStatus::Active {
                            return false;
                        }
                        match p.direction {
                            pkg_types::vpc::PeeringDirection::Bidirectional => {
                                (p.vpc_a == src_vpc && p.vpc_b == dst_vpc)
                                    || (p.vpc_a == dst_vpc && p.vpc_b == src_vpc)
                            }
                            pkg_types::vpc::PeeringDirection::InitiatorOnly => {
                                p.vpc_a == src_vpc && p.vpc_b == dst_vpc
                            }
                        }
                    });
                    VpcResponse::Reachable { reachable }
                }
                Err(e) => VpcResponse::Error {
                    code: "store_error".to_string(),
                    message: format!("Failed to load peerings: {}", e),
                },
            }
        }
    }
}
