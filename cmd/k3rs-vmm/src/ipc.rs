//! Unix domain socket IPC for exec forwarding between k3rs-vmm processes.
//!
//! The `boot` process creates a listener at `/tmp/k3rs-runtime/vms/vmm-{id}.sock`.
//! The `exec` subcommand connects to that socket, sends the command, and reads
//! the response. This allows exec to work even though the VM lives in a
//! separate long-running boot process.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::OnceLock;
use std::{io, thread};

use tracing::{error, info};

/// Socket directory for VM IPC
const SOCKET_DIR: &str = "/tmp/k3rs-runtime/vms";

/// Global VM ID for cleanup on exit (set by boot command).
static ACTIVE_VM_ID: OnceLock<String> = OnceLock::new();

/// Register the active VM ID so `cleanup()` knows which socket to remove.
pub fn set_active_vm(id: &str) {
    let _ = ACTIVE_VM_ID.set(id.to_string());
}

/// Clean up IPC socket for the active VM. Safe to call from any exit path
/// (signal handler, delegate, start_vm error handler, etc.).
pub fn cleanup() {
    if let Some(id) = ACTIVE_VM_ID.get() {
        let path = socket_path(id);
        let _ = std::fs::remove_file(&path);
        info!("cleaned up IPC socket: {}", path);
    }
}

/// Get the socket path for a given VM ID.
pub fn socket_path(id: &str) -> String {
    format!("{}/vmm-{}.sock", SOCKET_DIR, id)
}

/// Start an IPC listener for exec requests on the given VM.
///
/// This runs in a background thread and accepts connections on a Unix socket.
/// Each connection receives a NUL-delimited command string and responds with output.
pub fn start_listener(id: &str, exec_handler: impl Fn(&[String]) -> String + Send + 'static) {
    let path = socket_path(id);

    // Ensure socket directory exists
    let _ = std::fs::create_dir_all(SOCKET_DIR);

    // Remove stale socket
    let _ = std::fs::remove_file(&path);

    let listener = match std::os::unix::net::UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            error!("failed to bind IPC socket at {}: {}", path, e);
            return;
        }
    };

    info!("IPC listener started on {}", path);

    let id = id.to_string();
    thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(mut stream) => {
                    // Read command
                    let mut buf = Vec::new();
                    if let Err(e) = stream.read_to_end(&mut buf) {
                        error!("IPC read error: {}", e);
                        continue;
                    }

                    let cmd_string = match String::from_utf8(buf) {
                        Ok(s) => s,
                        Err(_) => continue,
                    };

                    let cmd_line = cmd_string.trim();
                    if cmd_line.is_empty() {
                        continue;
                    }

                    let parts: Vec<String> = cmd_line.split('\0').map(|s| s.to_string()).collect();
                    info!("IPC exec request for VM {}: {:?}", id, parts);

                    let output = exec_handler(&parts);
                    let _ = stream.write_all(output.as_bytes());
                }
                Err(e) => {
                    error!("IPC accept error: {}", e);
                }
            }
        }
    });
}

/// Stop the IPC listener by removing the socket file.
pub fn stop_listener(id: &str) {
    let path = socket_path(id);
    let _ = std::fs::remove_file(&path);
}

/// Connect to a running boot process's IPC socket and send an exec request.
pub fn exec_via_ipc(id: &str, command: &[String]) -> io::Result<String> {
    let path = socket_path(id);

    if !Path::new(&path).exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("VM '{}' not found (no socket at {})", id, path),
        ));
    }

    // Check if the boot process is actually alive before connecting
    if !is_vm_process_alive(id) {
        // Clean up stale socket
        let _ = std::fs::remove_file(&path);
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "VM '{}' boot process is not running (stale socket removed)",
                id
            ),
        ));
    }

    let mut stream = std::os::unix::net::UnixStream::connect(&path).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("failed to connect to VM '{}' IPC socket: {}", id, e),
        )
    })?;

    // Set a read timeout to avoid hanging forever if the boot process dies mid-connection
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;

    // Send command as NUL-delimited string + newline
    let cmd_string = command.join("\0") + "\n";
    stream.write_all(cmd_string.as_bytes())?;
    stream.shutdown(std::net::Shutdown::Write)?;

    // Read response
    let mut response = String::new();
    match stream.read_to_string(&mut response) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut => {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("VM '{}' exec timed out (boot process may have crashed)", id),
            ));
        }
        Err(e) => return Err(e),
    }

    if response.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionReset,
            format!(
                "VM '{}' returned empty response (boot process may have crashed)",
                id
            ),
        ));
    }

    Ok(response)
}

/// Check if the k3rs-vmm boot process for a given VM ID is alive.
fn is_vm_process_alive(id: &str) -> bool {
    let output = std::process::Command::new("pgrep")
        .args(["-f", &format!("k3rs-vmm boot.*--id {}", id)])
        .output();

    match output {
        Ok(o) if o.status.success() => {
            let pid_str = String::from_utf8_lossy(&o.stdout).trim().to_string();
            !pid_str.is_empty()
        }
        _ => false,
    }
}
