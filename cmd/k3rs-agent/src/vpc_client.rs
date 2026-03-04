//! Async Unix socket client for the `k3rs-vpc` daemon (NDJSON protocol).

use crate::connectivity::ConnectivityManager;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tracing::warn;

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
#[allow(dead_code)]
enum VpcRequest {
    Allocate {
        pod_id: String,
        vpc_name: String,
    },
    Release {
        pod_id: String,
        vpc_name: String,
    },
    GetRoutes {
        vpc_id: u16,
    },
    CheckReachability {
        src_vpc: String,
        dst_vpc: String,
    },
    AttachNetkit {
        nk_name: String,
        guest_ipv4: String,
        ghost_ipv6: String,
        vpc_id: u16,
        container_pid: u32,
    },
    DetachNetkit {
        nk_name: String,
    },
    AttachTap {
        tap_name: String,
        guest_ipv4: String,
        ghost_ipv6: String,
        vpc_id: u16,
    },
    DetachTap {
        tap_name: String,
    },
    ListVpcs,
    Ping,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct RouteEntry {
    pub destination: String,
    pub next_hop: String,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct VpcInfo {
    pub name: String,
    pub vpc_id: u16,
    pub ipv4_cidr: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum VpcResponse {
    Allocated {
        guest_ipv4: String,
        ghost_ipv6: String,
        vpc_id: u16,
    },
    Released,
    Routes {
        entries: Vec<RouteEntry>,
    },
    Reachable {
        reachable: bool,
    },
    VpcList {
        vpcs: Vec<VpcInfo>,
    },
    Pong,
    Ok,
    Error {
        code: String,
        message: String,
    },
}

#[allow(dead_code)]
pub struct VpcClient {
    socket_path: String,
}

impl VpcClient {
    pub fn new(socket_path: String) -> Self {
        Self { socket_path }
    }

    async fn request(&self, req: &VpcRequest) -> anyhow::Result<VpcResponse> {
        let stream = UnixStream::connect(&self.socket_path).await?;
        let (reader, mut writer) = stream.into_split();

        let mut line = serde_json::to_string(req)?;
        line.push('\n');
        writer.write_all(line.as_bytes()).await?;
        writer.shutdown().await?;

        let mut lines = BufReader::new(reader).lines();
        let resp_line = lines
            .next_line()
            .await?
            .ok_or_else(|| anyhow::anyhow!("VPC daemon closed connection without response"))?;

        let resp: VpcResponse = serde_json::from_str(&resp_line)?;
        Ok(resp)
    }

    /// Allocate a Ghost IPv6 address for a pod. Retries up to 5 times with backoff
    /// if the daemon is not yet ready (connection refused).
    pub async fn allocate(
        &self,
        pod_id: &str,
        vpc_name: &str,
    ) -> anyhow::Result<(String, String, u16)> {
        let req = VpcRequest::Allocate {
            pod_id: pod_id.to_string(),
            vpc_name: vpc_name.to_string(),
        };

        let mut last_err = None;
        for attempt in 0..5u32 {
            match self.request(&req).await {
                Ok(VpcResponse::Allocated {
                    guest_ipv4,
                    ghost_ipv6,
                    vpc_id,
                }) => return Ok((guest_ipv4, ghost_ipv6, vpc_id)),
                Ok(VpcResponse::Error { code, message }) => {
                    return Err(anyhow::anyhow!(
                        "VPC allocate error [{}]: {}",
                        code,
                        message
                    ));
                }
                Ok(other) => {
                    return Err(anyhow::anyhow!(
                        "Unexpected VPC response to Allocate: {:?}",
                        other
                    ));
                }
                Err(e) => {
                    warn!("VPC daemon not ready (attempt {}/5): {}", attempt + 1, e);
                    last_err = Some(e);
                    tokio::time::sleep(ConnectivityManager::backoff_duration(attempt)).await;
                }
            }
        }

        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("VPC allocate failed after 5 retries")))
    }

    /// Release a Ghost IPv6 address for a pod. Best-effort, single attempt.
    pub async fn release(&self, pod_id: &str, vpc_name: &str) -> anyhow::Result<()> {
        let req = VpcRequest::Release {
            pod_id: pod_id.to_string(),
            vpc_name: vpc_name.to_string(),
        };

        match self.request(&req).await {
            Ok(VpcResponse::Released) => Ok(()),
            Ok(VpcResponse::Error { code, message }) => {
                Err(anyhow::anyhow!("VPC release error [{}]: {}", code, message))
            }
            Ok(other) => Err(anyhow::anyhow!(
                "Unexpected VPC response to Release: {:?}",
                other
            )),
            Err(e) => Err(e),
        }
    }

    /// List all VPCs known to the VPC daemon.
    pub async fn list_vpcs(&self) -> anyhow::Result<Vec<VpcInfo>> {
        match self.request(&VpcRequest::ListVpcs).await {
            Ok(VpcResponse::VpcList { vpcs }) => Ok(vpcs),
            Ok(VpcResponse::Error { code, message }) => Err(anyhow::anyhow!(
                "VPC list_vpcs error [{}]: {}",
                code,
                message
            )),
            Ok(other) => Err(anyhow::anyhow!(
                "Unexpected VPC response to ListVpcs: {:?}",
                other
            )),
            Err(e) => Err(e),
        }
    }

    /// Get route entries (pod IPs) for a specific VPC by vpc_id.
    pub async fn get_routes(&self, vpc_id: u16) -> anyhow::Result<Vec<RouteEntry>> {
        match self.request(&VpcRequest::GetRoutes { vpc_id }).await {
            Ok(VpcResponse::Routes { entries }) => Ok(entries),
            Ok(VpcResponse::Error { code, message }) => Err(anyhow::anyhow!(
                "VPC get_routes error [{}]: {}",
                code,
                message
            )),
            Ok(other) => Err(anyhow::anyhow!(
                "Unexpected VPC response to GetRoutes: {:?}",
                other
            )),
            Err(e) => Err(e),
        }
    }

    /// Check if two VPCs can reach each other (via peering or same-VPC).
    #[allow(dead_code)]
    pub async fn check_reachability(&self, src_vpc: &str, dst_vpc: &str) -> anyhow::Result<bool> {
        let req = VpcRequest::CheckReachability {
            src_vpc: src_vpc.to_string(),
            dst_vpc: dst_vpc.to_string(),
        };

        match self.request(&req).await {
            Ok(VpcResponse::Reachable { reachable }) => Ok(reachable),
            Ok(VpcResponse::Error { code, message }) => Err(anyhow::anyhow!(
                "VPC check_reachability error [{}]: {}",
                code,
                message
            )),
            Ok(other) => Err(anyhow::anyhow!(
                "Unexpected VPC response to CheckReachability: {:?}",
                other
            )),
            Err(e) => Err(e),
        }
    }

    /// Attach tap_guard + IPv6 classifiers on a host-side TAP for a VM.
    pub async fn attach_tap(
        &self,
        tap_name: &str,
        guest_ipv4: &str,
        ghost_ipv6: &str,
        vpc_id: u16,
    ) -> anyhow::Result<()> {
        let req = VpcRequest::AttachTap {
            tap_name: tap_name.to_string(),
            guest_ipv4: guest_ipv4.to_string(),
            ghost_ipv6: ghost_ipv6.to_string(),
            vpc_id,
        };

        match self.request(&req).await {
            Ok(VpcResponse::Ok) => Ok(()),
            Ok(VpcResponse::Error { code, message }) => Err(anyhow::anyhow!(
                "VPC attach_tap error [{}]: {}",
                code,
                message
            )),
            Ok(other) => Err(anyhow::anyhow!(
                "Unexpected VPC response to AttachTap: {:?}",
                other
            )),
            Err(e) => Err(e),
        }
    }

    /// Detach TC classifiers from a TAP interface.
    #[allow(dead_code)]
    pub async fn detach_tap(&self, tap_name: &str) -> anyhow::Result<()> {
        let req = VpcRequest::DetachTap {
            tap_name: tap_name.to_string(),
        };

        match self.request(&req).await {
            Ok(VpcResponse::Ok) => Ok(()),
            Ok(VpcResponse::Error { code, message }) => Err(anyhow::anyhow!(
                "VPC detach_tap error [{}]: {}",
                code,
                message
            )),
            Ok(other) => Err(anyhow::anyhow!(
                "Unexpected VPC response to DetachTap: {:?}",
                other
            )),
            Err(e) => Err(e),
        }
    }

    /// Health check ping to the VPC daemon.
    #[allow(dead_code)]
    pub async fn ping(&self) -> anyhow::Result<()> {
        match self.request(&VpcRequest::Ping).await {
            Ok(VpcResponse::Pong) => Ok(()),
            Ok(VpcResponse::Error { code, message }) => {
                Err(anyhow::anyhow!("VPC ping error [{}]: {}", code, message))
            }
            Ok(other) => Err(anyhow::anyhow!(
                "Unexpected VPC response to Ping: {:?}",
                other
            )),
            Err(e) => Err(e),
        }
    }
}
