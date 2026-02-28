use anyhow::Result;
use async_trait::async_trait;
use std::path::Path;
use crate::backend::RuntimeBackend;
use crate::state::ContainerStateInfo;

/// Firecracker microVM backend for Linux.
///
/// Maps to the "vm" runtime alias on Linux.
pub struct FirecrackerBackend {
    #[allow(dead_code)]
    data_dir: std::path::PathBuf,
}

impl FirecrackerBackend {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
        }
    }
}

#[async_trait]
impl RuntimeBackend for FirecrackerBackend {
    fn name(&self) -> &str {
        "vm"
    }

    fn version(&self) -> &str {
        "firecracker-stub-1.0"
    }

    async fn create(&self, _id: &str, _bundle: &Path) -> Result<()> {
        anyhow::bail!("Firecracker backend is not fully implemented yet")
    }

    async fn start(&self, _id: &str) -> Result<()> {
        anyhow::bail!("Firecracker backend is not fully implemented yet")
    }

    async fn stop(&self, _id: &str) -> Result<()> {
        Ok(())
    }

    async fn delete(&self, _id: &str) -> Result<()> {
        Ok(())
    }

    async fn list(&self) -> Result<Vec<String>> {
        Ok(vec![])
    }

    async fn logs(&self, _id: &str, _tail: usize) -> Result<Vec<String>> {
        Ok(vec!["Firecracker logs not available yet".to_string()])
    }

    async fn exec(&self, _id: &str, _command: &[&str]) -> Result<String> {
        anyhow::bail!("Firecracker exec not implemented")
    }

    async fn spawn_exec(&self, _id: &str, _command: &[&str]) -> Result<tokio::process::Child> {
        anyhow::bail!("Firecracker spawn_exec not implemented")
    }

    async fn state(&self, id: &str) -> Result<ContainerStateInfo> {
        Err(anyhow::anyhow!("Firecracker state not implemented for {}", id))
    }
}
