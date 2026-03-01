//! Firecracker REST API client over Unix socket.
//!
//! Firecracker exposes an HTTP/1.1 API over a Unix domain socket.
//! This module provides a lightweight client without requiring hyper or
//! reqwest-unix — Firecracker's API is simple enough (PUT-only, JSON body)
//! that raw HTTP is straightforward.

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

/// Client for the Firecracker REST API.
pub struct FcApiClient {
    socket_path: String,
}

impl FcApiClient {
    pub fn new(socket_path: &str) -> Self {
        Self {
            socket_path: socket_path.to_string(),
        }
    }

    /// Send a PUT request to the Firecracker API and return the response body.
    ///
    /// Parses the HTTP response incrementally (headers → Content-Length → body)
    /// instead of relying on `shutdown()` + `read_to_end()`, which can race with
    /// Firecracker's micro_http server and cause connection resets.
    async fn put(&self, path: &str, body: &serde_json::Value) -> Result<String> {
        let mut stream = UnixStream::connect(&self.socket_path)
            .await
            .with_context(|| {
                format!(
                    "Failed to connect to Firecracker API socket at {}",
                    self.socket_path
                )
            })?;

        let body_str = serde_json::to_string(body)?;

        let request = format!(
            "PUT {path} HTTP/1.1\r\n\
             Host: localhost\r\n\
             Accept: application/json\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             \r\n\
             {body_str}",
            body_str.len(),
        );

        stream.write_all(request.as_bytes()).await?;

        let (status_code, response_body) = Self::read_http_response(&mut stream)
            .await
            .with_context(|| format!("PUT {}", path))?;

        if !(200..300).contains(&status_code) {
            anyhow::bail!(
                "Firecracker API error: HTTP {} for PUT {} — {}",
                status_code,
                path,
                response_body.trim()
            );
        }

        Ok(response_body)
    }

    /// Read a complete HTTP response by parsing headers to find Content-Length,
    /// then reading exactly that many body bytes. Uses a timeout to avoid blocking
    /// indefinitely if Firecracker crashes mid-response.
    async fn read_http_response(stream: &mut UnixStream) -> Result<(u16, String)> {
        let mut buf = Vec::with_capacity(4096);
        let mut tmp = [0u8; 4096];
        let timeout_dur = std::time::Duration::from_secs(30);

        loop {
            let n = tokio::time::timeout(timeout_dur, stream.read(&mut tmp))
                .await
                .context("Firecracker API response timeout (30s)")?
                .context("reading Firecracker API response")?;

            if n == 0 {
                // EOF before complete response
                break;
            }
            buf.extend_from_slice(&tmp[..n]);

            // Look for end-of-headers marker
            if let Some(hdr_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let header_str = String::from_utf8_lossy(&buf[..hdr_end]);

                let content_length: usize = header_str
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
                    .and_then(|l| l.split(':').nth(1))
                    .and_then(|s| s.trim().parse().ok())
                    .unwrap_or(0);

                let body_start = hdr_end + 4;
                let body_received = buf.len().saturating_sub(body_start);

                if body_received >= content_length {
                    // Complete response received — parse and return
                    let status: u16 = header_str
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(500);

                    let body = if content_length > 0 && body_start + content_length <= buf.len() {
                        String::from_utf8_lossy(&buf[body_start..body_start + content_length])
                            .to_string()
                    } else {
                        String::new()
                    };

                    return Ok((status, body));
                }
                // else: keep reading to get remaining body bytes
            }
        }

        // EOF before complete response
        let partial = String::from_utf8_lossy(&buf);
        anyhow::bail!(
            "Firecracker closed connection before sending a complete response: {}",
            partial.chars().take(200).collect::<String>()
        );
    }

    /// Configure VM machine resources (vCPUs + memory).
    pub async fn set_machine_config(&self, vcpu_count: u32, mem_size_mib: u64) -> Result<()> {
        self.put(
            "/machine-config",
            &serde_json::json!({
                "vcpu_count": vcpu_count,
                "mem_size_mib": mem_size_mib
            }),
        )
        .await?;
        Ok(())
    }

    /// Configure boot source (kernel image + boot arguments + optional initrd).
    pub async fn set_boot_source(
        &self,
        kernel_image_path: &str,
        boot_args: &str,
        initrd_path: Option<&str>,
    ) -> Result<()> {
        let mut body = serde_json::json!({
            "kernel_image_path": kernel_image_path,
            "boot_args": boot_args
        });
        if let Some(initrd) = initrd_path {
            body["initrd_path"] = serde_json::Value::String(initrd.to_string());
        }
        self.put("/boot-source", &body).await?;
        Ok(())
    }

    /// Add a block device (e.g., rootfs ext4 image).
    pub async fn add_drive(
        &self,
        drive_id: &str,
        path_on_host: &str,
        is_root_device: bool,
        is_read_only: bool,
    ) -> Result<()> {
        self.put(
            &format!("/drives/{}", drive_id),
            &serde_json::json!({
                "drive_id": drive_id,
                "path_on_host": path_on_host,
                "is_root_device": is_root_device,
                "is_read_only": is_read_only
            }),
        )
        .await?;
        Ok(())
    }

    /// Configure the vsock device for host↔guest communication.
    pub async fn set_vsock(&self, guest_cid: u32, uds_path: &str) -> Result<()> {
        self.put(
            "/vsock",
            &serde_json::json!({
                "guest_cid": guest_cid,
                "uds_path": uds_path
            }),
        )
        .await?;
        Ok(())
    }

    /// Add a network interface backed by a TAP device.
    pub async fn add_network_interface(
        &self,
        iface_id: &str,
        host_dev_name: &str,
    ) -> Result<()> {
        self.put(
            &format!("/network-interfaces/{}", iface_id),
            &serde_json::json!({
                "iface_id": iface_id,
                "host_dev_name": host_dev_name
            }),
        )
        .await?;
        Ok(())
    }

    /// Start the VM instance.
    pub async fn start_instance(&self) -> Result<()> {
        self.put(
            "/actions",
            &serde_json::json!({
                "action_type": "InstanceStart"
            }),
        )
        .await?;
        Ok(())
    }

    /// Send Ctrl+Alt+Del to gracefully shut down the guest.
    pub async fn send_ctrl_alt_del(&self) -> Result<()> {
        self.put(
            "/actions",
            &serde_json::json!({
                "action_type": "SendCtrlAltDel"
            }),
        )
        .await?;
        Ok(())
    }

    /// Get the full VM configuration (GET /machine-config).
    pub async fn get_machine_config(&self) -> Result<serde_json::Value> {
        let mut stream = UnixStream::connect(&self.socket_path).await?;

        let request = "GET /machine-config HTTP/1.1\r\n\
                       Host: localhost\r\n\
                       Accept: application/json\r\n\
                       \r\n";

        stream.write_all(request.as_bytes()).await?;

        let (_status, body) = Self::read_http_response(&mut stream).await?;
        Ok(serde_json::from_str(&body).unwrap_or(serde_json::json!({})))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_api_client_new() {
        let client = super::FcApiClient::new("/tmp/test.sock");
        assert_eq!(client.socket_path, "/tmp/test.sock");
    }
}
