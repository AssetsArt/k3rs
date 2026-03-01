//! Unix domain socket IPC for exec forwarding between k3rs-vmm processes.
//!
//! The `boot` process creates a listener at `/tmp/k3rs-runtime/vms/vmm-{id}.sock`.
//! The `exec` subcommand connects to that socket, sends the command, and reads
//! the response.
//!
//! ## Protocols
//!
//! ### Regular exec (one-shot)
//! 1. Client: `cmd\0arg1\0arg2\n` then `shutdown(Write)`
//! 2. Server: reads until EOF, calls exec_handler, writes response, closes
//!
//! ### Streaming exec (interactive / tty=true)
//! 1. Client: `\x01cmd\0arg1\0arg2\n` — the `\x01` prefix signals streaming mode
//! 2. Client keeps socket open; data after the command line is stdin for the guest
//! 3. Server: reads command (until `\n`), calls stream_handler(parts, socket)
//! 4. stream_handler relays the open socket ↔ vsock bidirectionally until done

use std::io::{Read, Write};
use std::os::unix::io::IntoRawFd;
use std::path::Path;
use std::sync::OnceLock;
use std::{io, thread};

use tracing::{error, info};

use pkg_constants::paths::VMM_SOCKET_DIR;

/// Byte prefix that distinguishes streaming exec from regular one-shot exec.
const STREAM_PREFIX: u8 = 0x01;

/// Read timeout for one-shot exec (waiting for the guest command to finish).
const EXEC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Global VM ID for cleanup on exit (set by boot command).
static ACTIVE_VM_ID: OnceLock<String> = OnceLock::new();

/// Register the active VM ID so `cleanup()` knows which socket to remove.
pub fn set_active_vm(id: &str) {
    let _ = ACTIVE_VM_ID.set(id.to_string());
}

/// Clean up IPC socket for the active VM. Safe to call from any exit path.
pub fn cleanup() {
    if let Some(id) = ACTIVE_VM_ID.get() {
        let path = socket_path(id);
        let _ = std::fs::remove_file(&path);
        info!("cleaned up IPC socket: {}", path);
    }
}

/// Get the socket path for a given VM ID.
pub fn socket_path(id: &str) -> String {
    format!("{}/vmm-{}.sock", VMM_SOCKET_DIR, id)
}

/// Start an IPC listener for exec requests on the given VM.
///
/// Two handler closures are accepted:
/// - `exec_handler`:   called for regular one-shot commands; returns output string.
/// - `stream_handler`: called for streaming/tty commands; receives the open
///                     `UnixStream` and is responsible for bidirectional relay.
pub fn start_listener(
    id: &str,
    exec_handler: impl Fn(&[String]) -> String + Send + Sync + 'static,
    stream_handler: impl Fn(&[String], std::os::unix::net::UnixStream) + Send + Sync + 'static,
) {
    let path = socket_path(id);

    let _ = std::fs::create_dir_all(VMM_SOCKET_DIR);
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

    use std::sync::Arc;
    let exec_handler = Arc::new(exec_handler);
    let stream_handler = Arc::new(stream_handler);

    thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let id = id.clone();
                    let exec_handler = Arc::clone(&exec_handler);
                    let stream_handler = Arc::clone(&stream_handler);

                    thread::spawn(move || {
                        handle_connection(stream, &id, &*exec_handler, &*stream_handler);
                    });
                }
                Err(e) => {
                    error!("IPC accept error: {}", e);
                }
            }
        }
    });
}

/// Handle a single IPC connection.
fn handle_connection(
    mut stream: std::os::unix::net::UnixStream,
    id: &str,
    exec_handler: &dyn Fn(&[String]) -> String,
    stream_handler: &dyn Fn(&[String], std::os::unix::net::UnixStream),
) {
    let mut first = [0u8; 1];
    if stream.read_exact(&mut first).is_err() {
        return;
    }

    if first[0] == STREAM_PREFIX {
        // ── Streaming mode ─────────────────────────────────────────────────────
        let mut cmd_buf = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            match stream.read_exact(&mut byte) {
                Ok(_) => {
                    if byte[0] == b'\n' {
                        break;
                    }
                    cmd_buf.push(byte[0]);
                }
                Err(_) => return,
            }
        }

        let cmd_str = match String::from_utf8(cmd_buf) {
            Ok(s) => s,
            Err(_) => {
                let _ = stream.write_all(b"exec error: invalid UTF-8 command\n");
                return;
            }
        };

        let parts: Vec<String> = cmd_str.split('\0').map(|s| s.to_string()).collect();
        if parts.is_empty() || parts[0].is_empty() {
            let _ = stream.write_all(b"exec error: empty command\n");
            return;
        }

        info!("IPC streaming exec for VM {}: {:?}", id, parts);
        stream_handler(&parts, stream);
    } else {
        // ── Regular one-shot mode ───────────────────────────────────────────────
        let mut rest = Vec::new();
        if let Err(e) = stream.read_to_end(&mut rest) {
            error!("IPC read_to_end error: {}", e);
            let _ = stream.write_all(b"exec error: IPC read failed\n");
            return;
        }

        let mut buf = vec![first[0]];
        buf.extend_from_slice(&rest);

        let cmd_string = match String::from_utf8(buf) {
            Ok(s) => s,
            Err(_) => {
                let _ = stream.write_all(b"exec error: invalid UTF-8 command\n");
                return;
            }
        };

        let cmd_line = cmd_string.trim();
        if cmd_line.is_empty() {
            let _ = stream.write_all(b"exec error: empty command\n");
            return;
        }

        let parts: Vec<String> = cmd_line.split('\0').map(|s| s.to_string()).collect();
        info!("IPC exec request for VM {}: {:?}", id, parts);

        let output = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            exec_handler(&parts)
        })) {
            Ok(out) => out,
            Err(_) => {
                error!("IPC exec handler panicked for VM {}", id);
                "exec error: handler panicked\n".to_string()
            }
        };
        let _ = stream.write_all(output.as_bytes());
    }
}

/// Stop the IPC listener by removing the socket file.
pub fn stop_listener(id: &str) {
    let path = socket_path(id);
    let _ = std::fs::remove_file(&path);
}

/// Connect to a running boot process's IPC socket and send a regular exec request.
pub fn exec_via_ipc(id: &str, command: &[String]) -> io::Result<String> {
    let mut stream = connect_to_ipc(id)?;

    // One-shot exec: set a read timeout so a hung guest doesn't block forever.
    stream.set_read_timeout(Some(EXEC_TIMEOUT))?;

    let cmd_string = command.join("\0") + "\n";
    stream.write_all(cmd_string.as_bytes())?;
    stream.shutdown(std::net::Shutdown::Write)?;

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

/// Connect to a running boot process's IPC socket and run a streaming PTY exec.
///
/// - Sends `\x01` + NUL-delimited command + `\n` to select streaming mode
/// - Puts the calling terminal into raw mode (so keystrokes are sent byte-by-byte)
/// - Relays stdin → socket and socket → stdout until the guest closes the connection
/// - Restores terminal state on exit
pub fn exec_streaming_via_ipc(id: &str, command: &[String]) -> io::Result<()> {
    // NOTE: no read timeout for streaming — the session can be idle indefinitely.
    let mut stream = connect_to_ipc(id)?;

    // Write streaming header directly to stream before consuming it.
    let mut header = vec![STREAM_PREFIX];
    header.extend_from_slice(command.join("\0").as_bytes());
    header.push(b'\n');
    stream.write_all(&header)?;

    // Consume the stream into a raw fd so the UnixStream wrapper won't try to
    // close it when it drops — the relay threads own the fd from here on.
    let stream_fd = stream.into_raw_fd();
    let stream_dup = unsafe { libc::dup(stream_fd) };
    if stream_dup < 0 {
        unsafe { libc::close(stream_fd) };
        return Err(io::Error::last_os_error());
    }

    // Put the host terminal into raw mode so keystrokes are forwarded byte-by-byte
    // (no line buffering, no echo, no special-character processing by the host).
    let saved_term = set_terminal_raw();

    // Self-pipe: t_out writes one byte here when the guest closes the connection,
    // which causes t_in's poll() to return so it can exit cleanly instead of
    // blocking forever on read(STDIN_FILENO).
    let mut done_pipe = [0i32; 2];
    if unsafe { libc::pipe(done_pipe.as_mut_ptr()) } < 0 {
        restore_terminal(saved_term);
        return Err(io::Error::last_os_error());
    }
    let done_read = done_pipe[0];
    let done_write = done_pipe[1];

    // Thread A: socket → stdout  (guest output → host terminal)
    let t_out = thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let stdout_fd = libc::STDOUT_FILENO;
        loop {
            let n =
                unsafe { libc::read(stream_fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n <= 0 {
                break;
            }
            let mut off = 0usize;
            while off < n as usize {
                let w = unsafe {
                    libc::write(
                        stdout_fd,
                        buf[off..n as usize].as_ptr() as *const libc::c_void,
                        n as usize - off,
                    )
                };
                if w <= 0 {
                    break;
                }
                off += w as usize;
            }
        }
        // Signal t_in that the session is over by closing the write end of the pipe.
        // t_in's poll() will see POLLHUP on done_read and exit its loop.
        unsafe { libc::close(done_write) };
        unsafe { libc::close(stream_fd) };
    });

    // Thread B: stdin → socket  (host terminal input → guest)
    // Uses poll() on both stdin and the done-pipe so it can exit immediately
    // when the guest closes the connection (t_out signals via done_write close).
    let t_in = thread::spawn(move || {
        let mut buf = [0u8; 256];
        let mut poll_fds = [
            libc::pollfd { fd: libc::STDIN_FILENO, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: done_read,           events: libc::POLLIN, revents: 0 },
        ];
        'outer: loop {
            let ready = unsafe { libc::poll(poll_fds.as_mut_ptr(), 2, -1) };
            if ready <= 0 {
                break;
            }
            // done_read fired (write end closed by t_out → session over)
            if poll_fds[1].revents != 0 {
                break;
            }
            // stdin has data
            if poll_fds[0].revents & libc::POLLIN != 0 {
                let n = unsafe {
                    libc::read(
                        libc::STDIN_FILENO,
                        buf.as_mut_ptr() as *mut libc::c_void,
                        buf.len(),
                    )
                };
                if n <= 0 {
                    break;
                }
                let mut off = 0usize;
                while off < n as usize {
                    let w = unsafe {
                        libc::write(
                            stream_dup,
                            buf[off..n as usize].as_ptr() as *const libc::c_void,
                            n as usize - off,
                        )
                    };
                    if w <= 0 {
                        break 'outer;
                    }
                    off += w as usize;
                }
                if off < n as usize {
                    break;
                }
            }
        }
        unsafe { libc::close(done_read) };
        // Signal to the guest that there is no more input.
        unsafe { libc::shutdown(stream_dup, libc::SHUT_WR) };
        unsafe { libc::close(stream_dup) };
    });

    t_out.join().ok();
    t_in.join().ok();

    // Restore terminal before returning.
    restore_terminal(saved_term);

    Ok(())
}

// ─── Terminal raw mode ────────────────────────────────────────────────────────

/// Switch the calling process's terminal to raw mode.
/// Returns the saved `termios` struct so the caller can restore it later.
fn set_terminal_raw() -> Option<libc::termios> {
    // Only do this if stdin is actually a terminal.
    if unsafe { libc::isatty(libc::STDIN_FILENO) } == 0 {
        return None;
    }

    let mut term: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut term) } != 0 {
        return None;
    }
    let saved = term;

    // cfmakeraw sets: no echo, no canonical mode, no signal chars, raw 8-bit input.
    unsafe {
        libc::cfmakeraw(&mut term);
        libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &term);
    }

    Some(saved)
}

/// Restore terminal settings saved by `set_terminal_raw`.
fn restore_terminal(saved: Option<libc::termios>) {
    if let Some(term) = saved {
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &term);
        }
    }
    // Print a newline so the restored prompt appears on a clean line.
    let _ = std::io::stdout().write_all(b"\r\n");
}

// ─── Internal helpers ─────────────────────────────────────────────────────────

/// Connect to the boot process's IPC socket.
/// Does NOT set a read timeout — callers are responsible for their own timeout policy.
fn connect_to_ipc(id: &str) -> io::Result<std::os::unix::net::UnixStream> {
    let path = socket_path(id);

    if !Path::new(&path).exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("VM '{}' not found (no socket at {})", id, path),
        ));
    }

    if !is_vm_process_alive(id) {
        let _ = std::fs::remove_file(&path);
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "VM '{}' boot process is not running (stale socket removed)",
                id
            ),
        ));
    }

    let stream =
        std::os::unix::net::UnixStream::connect(&path).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("failed to connect to VM '{}' IPC socket: {}", id, e),
            )
        })?;

    Ok(stream)
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
