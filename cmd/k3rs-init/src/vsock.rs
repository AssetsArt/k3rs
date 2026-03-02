use std::io::Write;
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
use std::process::Command;

use crate::is_initrd_mode;
use crate::VSOCK_EXEC_PORT;

/// Byte prefix that switches vsock exec into streaming PTY mode (must match k3rs-vmm).
const STREAM_PREFIX: u8 = 0x01;

/// Start a vsock listener for exec commands from the host (k3rs-vmm).
///
/// Listens on VSOCK_EXEC_PORT (5555) and for each connection:
/// 1. Reads a NUL-delimited command string
/// 2. Executes the command
/// 3. Sends stdout+stderr back to the host
/// 4. Closes the connection
#[cfg(target_os = "linux")]
pub fn start_vsock_listener() {
    std::thread::spawn(|| {
        if let Err(e) = vsock_listener_loop() {
            log_error!("vsock listener failed: {}", e);
        }
    });
    log_info!("vsock exec listener started on port {}", VSOCK_EXEC_PORT);
}

/// Main vsock listener loop.
#[cfg(target_os = "linux")]
fn vsock_listener_loop() -> Result<(), Box<dyn std::error::Error>> {
    #[allow(unused_imports)]
    use std::io::{Read, Write};

    // Create vsock socket
    // AF_VSOCK = 40, SOCK_STREAM = 1
    let sock = unsafe { libc::socket(40, libc::SOCK_STREAM, 0) };
    if sock < 0 {
        return Err("failed to create vsock socket".into());
    }

    // Bind to VMADDR_CID_ANY (u32::MAX = -1) on our exec port
    // struct sockaddr_vm { sa_family, reserved, port, cid }
    let mut addr: libc::sockaddr_vm = unsafe { std::mem::zeroed() };
    addr.svm_family = 40; // AF_VSOCK
    addr.svm_port = VSOCK_EXEC_PORT;
    addr.svm_cid = libc::VMADDR_CID_ANY;

    let ret = unsafe {
        libc::bind(
            sock,
            &addr as *const libc::sockaddr_vm as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_vm>() as u32,
        )
    };
    if ret < 0 {
        unsafe { libc::close(sock) };
        let err = std::io::Error::last_os_error();
        return Err(format!("vsock bind failed: {}", err).into());
    }

    // Listen
    if unsafe { libc::listen(sock, 5) } < 0 {
        unsafe { libc::close(sock) };
        return Err("vsock listen failed".into());
    }

    log_info!("vsock listening on port {}", VSOCK_EXEC_PORT);

    loop {
        let mut client_addr: libc::sockaddr_vm = unsafe { std::mem::zeroed() };
        let mut addr_len = std::mem::size_of::<libc::sockaddr_vm>() as u32;

        let client_fd = unsafe {
            libc::accept(
                sock,
                &mut client_addr as *mut libc::sockaddr_vm as *mut libc::sockaddr,
                &mut addr_len,
            )
        };
        if client_fd < 0 {
            continue;
        }

        // Handle exec in a new thread to not block the listener
        std::thread::spawn(move || {
            handle_vsock_exec(client_fd);
        });
    }
}

/// Handle a single vsock exec request.
///
/// Protocol detection (first byte):
/// - `\x01` → streaming PTY mode: create PTY, spawn command, bridge PTY ↔ vsock
/// - anything else → one-shot mode: run command, collect output, write, close
#[cfg(target_os = "linux")]
fn handle_vsock_exec(fd: i32) {
    // Read first byte to determine mode.
    let mut first = [0u8; 1];
    let n = unsafe { libc::read(fd, first.as_mut_ptr() as *mut libc::c_void, 1) };
    if n <= 0 {
        unsafe { libc::close(fd) };
        return;
    }

    let streaming = first[0] == STREAM_PREFIX;

    // Read command: if streaming, everything until '\n'; if one-shot, same but
    // prepend the first byte (it's the first char of the command).
    let mut cmd_buf = Vec::new();
    if !streaming {
        cmd_buf.push(first[0]);
    }
    let mut b = [0u8; 1];
    loop {
        let n = unsafe { libc::read(fd, b.as_mut_ptr() as *mut libc::c_void, 1) };
        if n <= 0 {
            break;
        }
        if b[0] == b'\n' {
            break;
        }
        cmd_buf.push(b[0]);
    }

    let input = String::from_utf8_lossy(&cmd_buf).to_string();
    let args: Vec<&str> = input.split('\0').collect();

    if args.is_empty() || args[0].is_empty() {
        let msg = b"error: empty command\n";
        unsafe { libc::write(fd, msg.as_ptr() as *const libc::c_void, msg.len()) };
        unsafe { libc::close(fd) };
        return;
    }

    if streaming {
        log_info!("vsock PTY exec: {:?}", args);
        handle_vsock_pty_exec(fd, &args);
    } else {
        log_info!("vsock exec: {:?}", args);

        // One-shot: run command, write combined output, close.
        let output = unsafe {
            Command::new(args[0])
                .args(&args[1..])
                .env(
                    "PATH",
                    "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                )
                .pre_exec(|| {
                    if is_initrd_mode() {
                        chroot_into_rootfs()
                    } else {
                        Ok(())
                    }
                })
                .output()
        };
        match output {
            Ok(out) => {
                log_info!(
                    "exec done: status={:?} stdout={} stderr={}",
                    out.status.code(),
                    out.stdout.len(),
                    out.stderr.len()
                );
                unsafe {
                    if !out.stdout.is_empty() {
                        let n = libc::write(
                            fd,
                            out.stdout.as_ptr() as *const libc::c_void,
                            out.stdout.len(),
                        );
                        log_info!("vsock write stdout: n={}", n);
                    }
                    if !out.stderr.is_empty() {
                        let n = libc::write(
                            fd,
                            out.stderr.as_ptr() as *const libc::c_void,
                            out.stderr.len(),
                        );
                        log_info!("vsock write stderr: n={}", n);
                    }
                }
            }
            Err(e) => {
                log_error!("exec failed: {}", e);
                let msg = format!("exec error: {}\n", e);
                unsafe { libc::write(fd, msg.as_ptr() as *const libc::c_void, msg.len()) };
            }
        }
        unsafe { libc::close(fd) };
    }
}

/// Chroot into the container rootfs if it is mounted at /mnt/rootfs.
/// Called via pre_exec in spawned command children.
#[cfg(target_os = "linux")]
pub fn chroot_into_rootfs() -> std::io::Result<()> {
    use std::ffi::CStr;
    let rootfs = b"/mnt/rootfs\0";
    let c_rootfs = unsafe { CStr::from_bytes_with_nul_unchecked(rootfs) };
    unsafe {
        let ret = libc::chroot(c_rootfs.as_ptr());
        if ret != 0 {
            return Err(std::io::Error::last_os_error());
        }
        libc::chdir(b"/\0".as_ptr() as *const libc::c_char);
    }
    Ok(())
}

/// Run `args` inside a PTY and bridge the PTY master ↔ vsock `fd` bidirectionally.
///
/// The shell sees a real terminal (prompts, job control, colours). Raw bytes flow:
///   host input  → vsock fd read  → PTY master write → shell stdin
///   shell output → PTY master read → vsock fd write → host
#[cfg(target_os = "linux")]
fn handle_vsock_pty_exec(vsock_fd: i32, args: &[&str]) {
    use std::os::unix::io::FromRawFd;

    let mut master: libc::c_int = -1;
    let mut slave: libc::c_int = -1;

    // Default window size: 80×24 ensures the shell formats output correctly.
    let winsize = libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let ret = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &winsize,
        )
    };

    if ret != 0 {
        let err = std::io::Error::last_os_error();
        let msg = format!("openpty error: {}\n", err);
        unsafe {
            libc::write(vsock_fd, msg.as_ptr() as *const libc::c_void, msg.len());
            libc::close(vsock_fd);
        }
        return;
    }

    // Spawn the command with the slave PTY as stdin/stdout/stderr.
    let child: Result<std::process::Child, _> = unsafe {
        use std::process::Stdio;
        Command::new(args[0])
            .args(&args[1..])
            .env("TERM", "xterm-256color")
            .env(
                "PATH",
                "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            )
            .stdin(Stdio::from_raw_fd(slave))
            .stdout(Stdio::from_raw_fd(libc::dup(slave)))
            .stderr(Stdio::from_raw_fd(libc::dup(slave)))
            .pre_exec(|| {
                if is_initrd_mode() {
                    chroot_into_rootfs()?;
                }
                libc::setsid();
                libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 0);
                Ok(())
            })
            .spawn()
    };

    // slave is owned by the child now; close our copy so EIO propagates correctly.
    unsafe { libc::close(slave) };

    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("spawn error: {}\n", e);
            unsafe {
                libc::write(vsock_fd, msg.as_ptr() as *const libc::c_void, msg.len());
                libc::close(master);
                libc::close(vsock_fd);
            }
            return;
        }
    };

    // Dup fds so each thread owns an independent handle.
    let master_read_fd = master;
    let master_write_fd = unsafe { libc::dup(master) };
    let vsock_read_fd = vsock_fd;
    let vsock_write_fd = unsafe { libc::dup(vsock_fd) };

    // Thread A: PTY master → vsock  (shell output → host)
    let t_pty_to_vsock = std::thread::spawn(move || {
        let mut buf = [0u8; 1024];
        loop {
            let n = unsafe {
                libc::read(
                    master_read_fd,
                    buf.as_mut_ptr() as *mut libc::c_void,
                    buf.len(),
                )
            };
            if n <= 0 {
                break; // EIO when shell exits and slave side closes
            }
            let mut off = 0usize;
            while off < n as usize {
                let w = unsafe {
                    libc::write(
                        vsock_write_fd,
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
        unsafe { libc::close(master_read_fd) };
        unsafe { libc::close(vsock_write_fd) };
    });

    // Thread B: vsock → PTY master  (host input → shell)
    let t_vsock_to_pty = std::thread::spawn(move || {
        let mut buf = [0u8; 1024];
        loop {
            let n = unsafe {
                libc::read(
                    vsock_read_fd,
                    buf.as_mut_ptr() as *mut libc::c_void,
                    buf.len(),
                )
            };
            if n <= 0 {
                break; // host disconnected
            }
            let mut off = 0usize;
            while off < n as usize {
                let w = unsafe {
                    libc::write(
                        master_write_fd,
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
        unsafe { libc::close(vsock_read_fd) };
        unsafe { libc::close(master_write_fd) };
    });

    // Wait for child to exit, then join threads.
    let _ = child.wait();
    let _ = t_pty_to_vsock.join();
    let _ = t_vsock_to_pty.join();
}
