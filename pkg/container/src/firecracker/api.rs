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
        stream.shutdown().await?;

        // Read response
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await?;
        let response_str = String::from_utf8_lossy(&response).to_string();

        // Parse HTTP status line
        let status_line = response_str
            .lines()
            .next()
            .unwrap_or("HTTP/1.1 500 Internal Server Error");

        let status_code: u16 = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(500);

        if !(200..300).contains(&status_code) {
            // Extract body after \r\n\r\n
            let body_part = response_str
                .split("\r\n\r\n")
                .nth(1)
                .unwrap_or("")
                .to_string();
            anyhow::bail!(
                "Firecracker API error: HTTP {} for PUT {} — {}",
                status_code,
                path,
                body_part.trim()
            );
        }

        // Extract response body
        let body_part = response_str
            .split("\r\n\r\n")
            .nth(1)
            .unwrap_or("")
            .to_string();

        Ok(body_part)
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
        stream.shutdown().await?;

        let mut response = Vec::new();
        stream.read_to_end(&mut response).await?;
        let response_str = String::from_utf8_lossy(&response).to_string();

        let body = response_str
            .split("\r\n\r\n")
            .nth(1)
            .unwrap_or("{}");

        Ok(serde_json::from_str(body).unwrap_or(serde_json::json!({})))
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
