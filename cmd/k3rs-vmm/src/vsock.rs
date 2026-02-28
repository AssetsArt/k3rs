//! Host→guest vsock exec via VZ framework's VZVirtioSocketDevice.
//!
//! k3rs-init listens on vsock port 5555 inside the guest for NUL-delimited
//! exec commands from the host. This module connects from the host using the
//! Virtualization.framework API and forwards commands, returning combined
//! stdout+stderr output.
//!
//! ## Protocol
//!
//! 1. Host sends: `arg0\0arg1\0arg2\n`  (NUL-delimited args, newline-terminated)
//! 2. Host shuts down write direction (signals end-of-input to guest)
//! 3. Guest reads args, executes command, writes stdout+stderr
//! 4. Guest closes connection
//! 5. Host reads to EOF, returns combined output

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

    // ── Phase 1: initiate connection on main thread ──────────────────────
    //
    // Bridge the async VZ completion handler to a synchronous result:
    //   Ok(fd)   – dup'd file descriptor we own exclusively
    //   Err(msg) – error description
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

        // We configured exactly one VZVirtioSocketDeviceConfiguration, so the
        // first device is a VZVirtioSocketDevice.
        let socket_device = devices.objectAtIndex(0);
        // Safety: the actual ObjC object is a VZVirtioSocketDevice (subclass of VZSocketDevice).
        let vsock_device: Retained<VZVirtioSocketDevice> =
            unsafe { Retained::cast_unchecked(socket_device) };

        // Build a heap-allocated block so VZ framework can copy it for the async callback.
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
                    // dup the fd so we own an independent handle.
                    // VZVirtioSocketConnection owns and will close the original fd
                    // when it is destroyed; our dup'd fd is unaffected.
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

        // Initiate async connection. The completion block is called on the main
        // queue once the connection is established (or fails).
        unsafe {
            vsock_device.connectToPort_completionHandler(VSOCK_EXEC_PORT, &*block);
        }
    });

    // ── Phase 2: wait for the dup'd fd ───────────────────────────────────
    let (lock, cv) = pair.as_ref();
    let guard = lock.lock().unwrap();
    let (mut guard, timed_out) = cv
        .wait_timeout_while(guard, VSOCK_CONNECT_TIMEOUT, |opt| opt.is_none())
        .unwrap();

    if timed_out.timed_out() {
        return "exec error: vsock connection timed out (is guest booted and ready?)\n"
            .to_string();
    }

    let fd = match guard.take().unwrap() {
        Ok(fd) => fd,
        Err(e) => return format!("exec error: {}\n", e),
    };
    drop(guard);

    // ── Phase 3: write command + read response ───────────────────────────
    exec_on_fd(fd, &cmd_str)
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

    // Shutdown write direction so k3rs-init knows the full request has arrived
    unsafe { libc::shutdown(fd, libc::SHUT_WR) };

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
