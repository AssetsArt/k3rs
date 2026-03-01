//! Host→guest vsock exec via VZ framework's VZVirtioSocketDevice.
//!
//! k3rs-init listens on vsock port 5555 inside the guest for NUL-delimited
//! exec commands from the host. This module connects from the host using the
//! Virtualization.framework API and forwards commands.
//!
//! ## Protocols
//!
//! ### One-shot exec (port 5555, no prefix)
//! 1. Host sends: `arg0\0arg1\0arg2\n`  (NUL-delimited args, newline-terminated)
//! 2. Host shuts down write direction (signals end-of-input to guest)
//! 3. Guest reads args, executes command, writes stdout+stderr
//! 4. Guest closes connection
//! 5. Host reads to EOF, returns combined output
//!
//! ### Streaming PTY exec (port 5555, `\x01` prefix)
//! 1. Host sends: `\x01arg0\0arg1\0arg2\n`  (`\x01` = streaming indicator)
//! 2. Host keeps socket open for bidirectional relay
//! 3. Guest reads command, creates PTY, spawns command with PTY slave
//! 4. Guest bridges PTY master ↔ vsock bidirectionally
//! 5. When guest process exits, guest closes connection
//! 6. Host detects EOF, closes IPC relay

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use block2::RcBlock;
use dispatch2::{MainThreadBound, run_on_main};
use objc2::rc::Retained;
use objc2_foundation::NSError;
use objc2_virtualization::{VZVirtualMachine, VZVirtioSocketConnection, VZVirtioSocketDevice};
use tracing::{error, info};

/// vsock port k3rs-init listens on for exec commands.
const VSOCK_EXEC_PORT: u32 = 5555;

/// How long to wait for a vsock connection to be established.
const VSOCK_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Execute a command in the guest via vsock, returning combined stdout+stderr.
///
/// Must be called from a **non-main** thread (e.g. the IPC thread).
/// Internally uses `run_on_main` to invoke the VZ framework's async
/// `connectToPort:completionHandler:`, then waits synchronously via a condvar.
pub fn exec_via_vsock(
    vm: &Arc<MainThreadBound<Retained<VZVirtualMachine>>>,
    command: &[String],
) -> String {
    if command.is_empty() {
        return "exec error: empty command\n".to_string();
    }

    info!("vsock exec: {:?}", command);

    // NUL-delimited command + newline terminator (matches k3rs-init vsock protocol)
    let cmd_str = command.join("\0") + "\n";

    match connect_vsock(vm) {
        Ok(fd) => exec_on_fd(fd, &cmd_str),
        Err(e) => format!("exec error: {}\n", e),
    }
}

/// Execute a streaming PTY command in the guest via vsock.
///
/// Sends `\x01` + NUL-delimited command to k3rs-init's PTY listener, then
/// relays `ipc_stream` ↔ vsock bidirectionally until the guest closes the
/// connection (process exited).
///
/// `ipc_stream` is the open UnixStream from the IPC listener, already past the
/// command-header bytes (it carries stdin/stdout for the exec session).
pub fn exec_streaming_via_vsock(
    vm: &Arc<MainThreadBound<Retained<VZVirtualMachine>>>,
    command: &[String],
    ipc_stream: std::os::unix::net::UnixStream,
) {
    if command.is_empty() {
        error!("exec_streaming_via_vsock: empty command");
        return;
    }

    info!("vsock streaming exec: {:?}", command);

    let fd = match connect_vsock(vm) {
        Ok(f) => f,
        Err(e) => {
            error!("vsock connect failed: {}", e);
            return;
        }
    };

    // Send streaming prefix + NUL-delimited command + newline.
    // The `\x01` byte tells k3rs-init to use PTY streaming mode.
    let mut cmd_bytes = vec![0x01u8];
    cmd_bytes.extend_from_slice(command.join("\0").as_bytes());
    cmd_bytes.push(b'\n');

    let mut written = 0;
    while written < cmd_bytes.len() {
        let n = unsafe {
            libc::write(
                fd,
                cmd_bytes[written..].as_ptr() as *const libc::c_void,
                cmd_bytes.len() - written,
            )
        };
        if n <= 0 {
            error!("vsock write failed: {}", std::io::Error::last_os_error());
            unsafe { libc::close(fd) };
            return;
        }
        written += n as usize;
    }

    // Bidirectional relay: ipc_stream ↔ vsock fd
    use std::os::unix::io::IntoRawFd;
    let ipc_fd = ipc_stream.into_raw_fd(); // we now own this fd exclusively
    let ipc_fd_dup = unsafe { libc::dup(ipc_fd) };
    let vsock_fd_dup = unsafe { libc::dup(fd) };

    // Thread A: vsock → ipc  (guest output → exec subprocess stdout)
    let t_vsock_to_ipc = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            let n =
                unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n <= 0 {
                break;
            }
            let mut off = 0usize;
            while off < n as usize {
                let w = unsafe {
                    libc::write(
                        ipc_fd,
                        buf[off..n as usize].as_ptr() as *const libc::c_void,
                        n as usize - off,
                    )
                };
                if w <= 0 {
                    break;
                }
                off += w as usize;
            }
            if off < n as usize {
                break;
            }
        }
        // Signal to exec subprocess that there is no more output.
        unsafe { libc::shutdown(ipc_fd, libc::SHUT_WR) };
        unsafe { libc::close(ipc_fd) };
        unsafe { libc::close(fd) };
    });

    // Thread B: ipc → vsock  (exec subprocess stdin → guest input)
    let t_ipc_to_vsock = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            let n = unsafe {
                libc::read(
                    ipc_fd_dup,
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
                        vsock_fd_dup,
                        buf[off..n as usize].as_ptr() as *const libc::c_void,
                        n as usize - off,
                    )
                };
                if w <= 0 {
                    break;
                }
                off += w as usize;
            }
            if off < n as usize {
                break;
            }
        }
        // Close vsock write side so guest sees EOF on its read path.
        unsafe { libc::shutdown(vsock_fd_dup, libc::SHUT_WR) };
        unsafe { libc::close(ipc_fd_dup) };
        unsafe { libc::close(vsock_fd_dup) };
    });

    t_vsock_to_ipc.join().ok();
    t_ipc_to_vsock.join().ok();
}

// ─── Internal helpers ─────────────────────────────────────────────────────────

/// Connect to vsock port 5555 in the guest, returning a dup'd file descriptor.
///
/// Must be called from a non-main thread; uses `run_on_main` internally.
fn connect_vsock(
    vm: &Arc<MainThreadBound<Retained<VZVirtualMachine>>>,
) -> Result<i32, String> {
    type Pair = (Mutex<Option<Result<i32, String>>>, Condvar);
    let pair: Arc<Pair> = Arc::new((Mutex::new(None), Condvar::new()));
    let pair_for_block = Arc::clone(&pair);

    let vm_clone = Arc::clone(vm);
    run_on_main(|marker| {
        let vm_ref = vm_clone.get(marker);
        let devices = unsafe { vm_ref.socketDevices() };

        if devices.is_empty() {
            let (lock, cv) = pair_for_block.as_ref();
            *lock.lock().unwrap() = Some(Err("VM has no vsock devices".to_string()));
            cv.notify_all();
            return;
        }

        let socket_device = devices.objectAtIndex(0);
        let vsock_device: Retained<VZVirtioSocketDevice> =
            unsafe { Retained::cast_unchecked(socket_device) };

        let pair_block = Arc::clone(&pair_for_block);
        let block = RcBlock::new(
            move |conn: *mut VZVirtioSocketConnection, err: *mut NSError| {
                let (lock, cv) = pair_block.as_ref();
                let mut guard = lock.lock().unwrap();

                if !err.is_null() {
                    let desc = unsafe { (*err).localizedDescription() };
                    *guard = Some(Err(format!("vsock connect failed: {}", desc)));
                } else if conn.is_null() {
                    *guard = Some(Err("vsock connection is null".to_string()));
                } else {
                    let fd = unsafe { (*conn).fileDescriptor() };
                    let dup_fd = unsafe { libc::dup(fd) };
                    if dup_fd < 0 {
                        let e = std::io::Error::last_os_error();
                        *guard = Some(Err(format!("dup(vsock fd) failed: {}", e)));
                    } else {
                        *guard = Some(Ok(dup_fd));
                    }
                }

                cv.notify_all();
            },
        );

        unsafe {
            vsock_device.connectToPort_completionHandler(VSOCK_EXEC_PORT, &*block);
        }
    });

    let (lock, cv) = pair.as_ref();
    let guard = lock.lock().unwrap();
    let (mut guard, timed_out) = cv
        .wait_timeout_while(guard, VSOCK_CONNECT_TIMEOUT, |opt| opt.is_none())
        .unwrap();

    if timed_out.timed_out() {
        return Err("vsock connection timed out (is guest booted and ready?)".to_string());
    }

    guard.take().unwrap()
}

/// Write `cmd` to `fd`, shutdown write direction, read all response bytes, close `fd`.
fn exec_on_fd(fd: i32, cmd: &str) -> String {
    let bytes = cmd.as_bytes();
    let mut written = 0;

    // Write the full command
    while written < bytes.len() {
        let n = unsafe {
            libc::write(
                fd,
                bytes[written..].as_ptr() as *const libc::c_void,
                bytes.len() - written,
            )
        };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            error!("vsock write failed: {}", e);
            unsafe { libc::close(fd) };
            return format!("exec error: write failed: {}\n", e);
        }
        written += n as usize;
    }

    // NOTE: do NOT shutdown(SHUT_WR) here.  Apple's Virtualization.framework
    // vsock does not support half-duplex shutdown — calling shutdown(SHUT_WR)
    // closes the entire connection, causing the guest's write-back to fail
    // silently and the host to read an empty response.  The '\n' terminator
    // already tells k3rs-init that the full request has arrived.

    // Read response until EOF (k3rs-init closes the connection after writing output)
    let mut output = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        match n {
            0 => break, // EOF — guest closed connection after writing output
            n if n > 0 => output.extend_from_slice(&buf[..n as usize]),
            _ => {
                let e = std::io::Error::last_os_error();
                if e.kind() != std::io::ErrorKind::Interrupted {
                    error!("vsock read failed: {}", e);
                    break;
                }
            }
        }
    }

    unsafe { libc::close(fd) };
    String::from_utf8_lossy(&output).into_owned()
}
