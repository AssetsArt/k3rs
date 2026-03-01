use crate::backend::RuntimeBackend;
use crate::state::ContainerStateInfo;
use anyhow::Result;
use async_trait::async_trait;
use std::path::{Path, PathBuf};

/// Firecracker microVM backend for Linux.
///
/// Maps to the "vm" runtime alias on Linux.
///
/// # Process Independence (not yet implemented)
///
/// When `spawn_firecracker()` is wired up it MUST ensure the Firecracker /
/// Jailer process is fully independent of the agent so VMs survive agent
/// restarts.  The required pattern mirrors `VirtualizationBackend::boot_vm()`:
///
/// ```rust,ignore
/// use std::os::unix::process::CommandExt;
///
/// // 1. Detach from the agent's session (no SIGHUP on terminal close / agent exit)
/// unsafe {
///     cmd.pre_exec(|| { libc::setsid(); Ok(()) });
/// }
///
/// // 2. No controlling terminal inheritance
/// cmd.stdin(std::process::Stdio::null());
///
/// // 3. Write a PID file after spawn — used by restore_from_pid_files() for
/// //    liveness-checked rediscovery on the next agent start.
/// std::fs::write(pid_file_path(id), format!("{}\n", pid))?;
/// ```
///
/// Additionally, when using the Firecracker Jailer:
/// - The Jailer already calls `unshare(CLONE_NEWPID)` internally, providing
///   an extra isolation layer on top of setsid().
/// - Pass `--daemonize` to the Jailer (or double-fork manually) so the Jailer
///   parent exits immediately, making Firecracker an orphan reparented to PID 1.
pub struct FirecrackerBackend {
    #[allow(dead_code)]
    data_dir: PathBuf,
}

impl FirecrackerBackend {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
        }
    }

    /// Spawn a Firecracker (or Jailer-wrapped Firecracker) process for the VM.
    ///
    /// **Not yet implemented.**  When implemented this method must:
    ///
    /// 1. Call `setsid()` via `pre_exec` to detach from the agent's session.
    /// 2. Redirect stdin to `/dev/null`.
    /// 3. Write `{data_dir}/vms/{id}.pid` after a successful spawn.
    /// 4. Optionally call `jailer` with `--daemonize` for deeper isolation.
    ///
    /// See the struct-level doc comment for the required code pattern.
    #[allow(dead_code)]
    fn spawn_firecracker(&self, id: &str) -> Result<u32> {
        // TODO: implement with:
        //   1. pre_exec setsid() for process group independence
        //   2. stdin → /dev/null (no controlling terminal)
        //   3. PID file write at {data_dir}/vms/{id}.pid
        //   4. Optional: Jailer with --daemonize
        anyhow::bail!(
            "Firecracker backend not implemented for VM {} — \
             see spawn_firecracker() doc comment for required process-independence pattern",
            id
        )
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

    async fn spawn_exec(
        &self,
        _id: &str,
        _command: &[&str],
        _tty: bool,
    ) -> Result<tokio::process::Child> {
        anyhow::bail!("Firecracker spawn_exec not implemented")
    }

    async fn state(&self, id: &str) -> Result<ContainerStateInfo> {
        Err(anyhow::anyhow!(
            "Firecracker state not implemented for {}",
            id
        ))
    }
}
