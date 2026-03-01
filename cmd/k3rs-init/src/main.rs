//! # k3rs-init — Guest PID 1
//!
//! This binary runs as `/sbin/init` inside lightweight Linux microVMs (both
//! macOS Virtualization.framework and Linux Firecracker/rust-vmm backends).
//!
//! ## Responsibilities
//! 1. Mount essential pseudo-filesystems (`/proc`, `/sys`, `/dev`, `/tmp`, `/run`)
//! 2. Setup basic networking (loopback `lo`, `eth0`)
//! 3. Set hostname
//! 4. Reap orphaned zombie processes (critical for PID 1)
//! 5. Parse OCI `config.json` and exec the container entrypoint
//!
//! ## Build
//! ```bash
//! # x86_64
//! cargo build --release --target x86_64-unknown-linux-musl -p k3rs-init
//! # aarch64
//! cargo build --release --target aarch64-unknown-linux-musl -p k3rs-init
//! ```
//!
//! ## Note
//! This binary is strictly Linux-only. On macOS it compiles with stubs for CI,
//! but should **never** be run outside a Linux guest VM.

// On non-Linux targets, all the Linux-specific code is cfg'd out,
// making types/constants appear unused. Suppress those warnings.
#![cfg_attr(not(target_os = "linux"), allow(unused))]

use serde::Deserialize;
use std::io::Write;
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

// ============================================================
// OCI Runtime Spec (minimal subset of config.json)
// ============================================================

/// Minimal OCI runtime spec — only the fields we need.
#[derive(Debug, Deserialize)]
struct OciSpec {
    #[serde(default)]
    process: Option<OciProcess>,
    #[serde(default)]
    hostname: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OciProcess {
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: Vec<String>,
    #[serde(default)]
    cwd: Option<String>,
}

// ============================================================
// Logging helpers (stdout/stderr → virtio-console)
// ============================================================

macro_rules! log_info {
    ($($arg:tt)*) => {
        let _ = writeln!(std::io::stdout(), "[k3rs-init] {}", format!($($arg)*));
    };
}

macro_rules! log_error {
    ($($arg:tt)*) => {
        let _ = writeln!(std::io::stderr(), "[k3rs-init] ERROR: {}", format!($($arg)*));
    };
}

// ============================================================
// Constants
// ============================================================

/// Paths where the host may mount the OCI config via virtio-fs or 9p.
const CONFIG_PATHS: &[&str] = &[
    "/run/config.json",
    "/mnt/config.json",
    "/config.json",
    "/mnt/rootfs/config.json", // virtio-fs mounted rootfs
];

const DEFAULT_HOSTNAME: &str = "k3rs-guest";

/// virtio-fs tag used by k3rs-vmm for rootfs sharing.
const VIRTIOFS_TAG: &str = "rootfs";

/// vsock port for exec commands from host (k3rs-vmm).
const VSOCK_EXEC_PORT: u32 = 5555;

// ============================================================
// Boot mode detection
// ============================================================

/// True when k3rs-init is running from an initrd (must mount virtiofs + chroot).
/// False when the kernel has already mounted virtiofs as the root filesystem.
static INITRD_MODE: OnceLock<bool> = OnceLock::new();

fn is_initrd_mode() -> bool {
    *INITRD_MODE.get().unwrap_or(&false)
}

// ============================================================
// Main
// ============================================================

fn main() {
    log_info!("starting (pid={})", std::process::id());

    // Steps 1-5 are Linux-only
    #[cfg(target_os = "linux")]
    linux_main();

    #[cfg(not(target_os = "linux"))]
    {
        log_error!("k3rs-init is a Linux-only binary — cannot run on this platform");
        std::process::exit(1);
    }
}

#[cfg(target_os = "linux")]
fn linux_main() {
    // 1. Mount pseudo-filesystems
    if let Err(e) = mount_filesystems() {
        log_error!("failed to mount filesystems: {}", e);
    }

    // Detect boot mode: if /config.json already exists we are running directly
    // from the container rootfs (no-initrd). If not, we are in the initrd and
    // must mount virtiofs before we can reach the container filesystem.
    let initrd_mode = !Path::new("/config.json").exists();
    let _ = INITRD_MODE.set(initrd_mode);

    // 1b. Mount virtio-fs shared rootfs from host (initrd mode only)
    if initrd_mode {
        mount_virtiofs();
    } else {
        log_info!("no-initrd mode: virtiofs is already the root filesystem");
    }

    // 2. Set hostname
    if let Err(e) = nix::unistd::sethostname(DEFAULT_HOSTNAME) {
        log_error!("failed to set hostname: {}", e);
    } else {
        log_info!("hostname set to '{}'", DEFAULT_HOSTNAME);
    }

    // 3. Setup networking
    if let Err(e) = setup_networking() {
        log_error!("failed to setup networking: {}", e);
    }

    // 4. Install signal handlers
    install_signal_handlers();

    // 4b. Start vsock exec listener (background thread)
    start_vsock_listener();

    // 5. Parse OCI config and execute entrypoint
    match load_oci_config() {
        Ok(spec) => {
            // Override hostname if OCI spec provides one
            if let Some(ref h) = spec.hostname {
                if h != DEFAULT_HOSTNAME {
                    let _ = nix::unistd::sethostname(h);
                }
            }

            if let Some(process) = spec.process {
                run_entrypoint(process);
            } else {
                log_error!("no 'process' section in OCI config — dropping to reaper loop");
                reaper_loop();
            }
        }
        Err(e) => {
            log_error!("failed to load OCI config: {} — dropping to reaper loop", e);
            reaper_loop();
        }
    }
}

// ============================================================
// 1b. Mount virtio-fs shared rootfs from host (Linux only)
// ============================================================

/// Mount the virtio-fs shared directory from the host.
///
/// k3rs-vmm shares the container rootfs directory via virtio-fs with tag "rootfs".
/// We mount it at /mnt/rootfs so the container filesystem from the host is accessible.
#[cfg(target_os = "linux")]
fn mount_virtiofs() {
    use std::ffi::CString;
    use std::fs;

    let mount_point = "/mnt/rootfs";
    if let Err(e) = fs::create_dir_all(mount_point) {
        log_error!("failed to create virtio-fs mount point: {}", e);
        return;
    }

    let c_source = match CString::new(VIRTIOFS_TAG) {
        Ok(s) => s,
        Err(_) => return,
    };
    let c_target = match CString::new(mount_point) {
        Ok(s) => s,
        Err(_) => return,
    };
    let c_fstype = match CString::new("virtiofs") {
        Ok(s) => s,
        Err(_) => return,
    };

    let ret = unsafe {
        libc::mount(
            c_source.as_ptr(),
            c_target.as_ptr(),
            c_fstype.as_ptr(),
            0,
            std::ptr::null(),
        )
    };

    if ret == 0 {
        log_info!("virtio-fs '{}' mounted at {}", VIRTIOFS_TAG, mount_point);
    } else {
        let err = std::io::Error::last_os_error();
        // Not a hard failure — VM might not have virtio-fs configured
        log_info!(
            "virtio-fs mount skipped ({}), not available in this VM",
            err
        );
    }
}

// ============================================================
// 4b. vsock exec listener (Linux only)
// ============================================================

/// Start a vsock listener for exec commands from the host (k3rs-vmm).
///
/// Listens on VSOCK_EXEC_PORT (5555) and for each connection:
/// 1. Reads a NUL-delimited command string
/// 2. Executes the command
/// 3. Sends stdout+stderr back to the host
/// 4. Closes the connection
#[cfg(target_os = "linux")]
fn start_vsock_listener() {
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

/// Byte prefix that switches vsock exec into streaming PTY mode (must match k3rs-vmm).
const STREAM_PREFIX: u8 = 0x01;

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
        // In initrd mode chroot into the virtiofs-mounted container rootfs first;
        // in no-initrd mode virtiofs IS already the root so no chroot is needed.
        //
        // Always set a standard PATH so bare command names (e.g. "ls") resolve
        // correctly inside the chroot even if PID 1 was started with no PATH.
        let output = unsafe {
            Command::new(args[0]).args(&args[1..])
                .env("PATH", "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin")
                .pre_exec(|| {
                    if is_initrd_mode() { chroot_into_rootfs() } else { Ok(()) }
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
fn chroot_into_rootfs() -> std::io::Result<()> {
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

    let ret = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
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
    // setsid() + TIOCSCTTY give the shell a proper controlling terminal.
    let child: Result<std::process::Child, _> = unsafe {
        use std::process::Stdio;
        Command::new(args[0])
            .args(&args[1..])
            .env("TERM", "xterm-256color")
            .env("PATH", "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin")
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

// ============================================================
// 1. Mount pseudo-filesystems (Linux only)
// ============================================================

#[cfg(target_os = "linux")]
fn mount_filesystems() -> Result<(), Box<dyn std::error::Error>> {
    use std::ffi::CString;
    use std::fs;

    // Create mount points if they don't exist
    let dirs = [
        "/proc", "/sys", "/dev", "/dev/pts", "/dev/shm", "/tmp", "/run",
    ];
    for dir in &dirs {
        if !Path::new(dir).exists() {
            fs::create_dir_all(dir)?;
        }
    }

    // Mount table: (source, target, fstype, flags)
    let ms_nosuid = libc::MS_NOSUID as u64;
    let ms_nodev = libc::MS_NODEV as u64;
    let ms_noexec = libc::MS_NOEXEC as u64;

    let mounts: &[(&str, &str, &str, u64)] = &[
        ("proc", "/proc", "proc", ms_nosuid | ms_nodev | ms_noexec),
        ("sysfs", "/sys", "sysfs", ms_nosuid | ms_nodev | ms_noexec),
        ("devtmpfs", "/dev", "devtmpfs", ms_nosuid),
        ("devpts", "/dev/pts", "devpts", ms_nosuid | ms_noexec),
        ("tmpfs", "/dev/shm", "tmpfs", ms_nosuid | ms_nodev),
        ("tmpfs", "/tmp", "tmpfs", ms_nosuid | ms_nodev),
        ("tmpfs", "/run", "tmpfs", ms_nosuid | ms_nodev | ms_noexec),
    ];

    for (source, target, fstype, flags) in mounts {
        if is_mounted(target) {
            continue;
        }

        let c_source = CString::new(*source)?;
        let c_target = CString::new(*target)?;
        let c_fstype = CString::new(*fstype)?;

        let ret = unsafe {
            libc::mount(
                c_source.as_ptr(),
                c_target.as_ptr(),
                c_fstype.as_ptr(),
                *flags,
                std::ptr::null(),
            )
        };

        if ret != 0 {
            let err = std::io::Error::last_os_error();
            log_error!("mount {} on {} failed: {}", source, target, err);
        }

        // After devtmpfs replaces /dev, recreate mount points for devpts/shm.
        if *target == "/dev" && ret == 0 {
            let _ = fs::create_dir_all("/dev/pts");
            let _ = fs::create_dir_all("/dev/shm");
        }
    }

    // Create standard symlinks in /dev
    create_dev_symlinks();

    log_info!("pseudo-filesystems mounted");
    Ok(())
}

#[cfg(target_os = "linux")]
fn is_mounted(target: &str) -> bool {
    std::fs::read_to_string("/proc/mounts")
        .unwrap_or_default()
        .lines()
        .any(|line| {
            line.split_whitespace()
                .nth(1)
                .map_or(false, |mp| mp == target)
        })
}

#[cfg(target_os = "linux")]
fn create_dev_symlinks() {
    let symlinks = [
        ("/proc/self/fd", "/dev/fd"),
        ("/proc/self/fd/0", "/dev/stdin"),
        ("/proc/self/fd/1", "/dev/stdout"),
        ("/proc/self/fd/2", "/dev/stderr"),
    ];
    for (src, dst) in &symlinks {
        if !Path::new(dst).exists() {
            let _ = std::os::unix::fs::symlink(src, dst);
        }
    }
}

// ============================================================
// 2. Networking setup (Linux only)
// ============================================================

#[cfg(target_os = "linux")]
fn setup_networking() -> Result<(), Box<dyn std::error::Error>> {
    // Bring up loopback
    if Path::new("/sys/class/net/lo/operstate").exists() {
        bring_interface_up("lo")?;
        log_info!("loopback interface up");
    }

    // Bring up eth0 (virtio-net)
    if Path::new("/sys/class/net/eth0/operstate").exists() {
        bring_interface_up("eth0")?;
        log_info!("eth0 interface up");
    }

    Ok(())
}

/// Bring a network interface up using raw socket ioctl (no `ip` binary needed).
#[cfg(target_os = "linux")]
fn bring_interface_up(iface: &str) -> Result<(), Box<dyn std::error::Error>> {
    let sock = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
    if sock < 0 {
        return Err(format!("socket() failed for {}", iface).into());
    }

    let mut ifr: libc::ifreq = unsafe { std::mem::zeroed() };
    let name_bytes = iface.as_bytes();
    let copy_len = name_bytes.len().min(libc::IFNAMSIZ - 1);
    unsafe {
        std::ptr::copy_nonoverlapping(
            name_bytes.as_ptr(),
            ifr.ifr_name.as_mut_ptr() as *mut u8,
            copy_len,
        );
    }

    // Get current flags
    if unsafe { libc::ioctl(sock, libc::SIOCGIFFLAGS as _, &mut ifr) } < 0 {
        unsafe { libc::close(sock) };
        return Err(format!("SIOCGIFFLAGS failed for {}", iface).into());
    }

    // Set IFF_UP | IFF_RUNNING
    unsafe {
        ifr.ifr_ifru.ifru_flags |= (libc::IFF_UP | libc::IFF_RUNNING) as i16;
    }

    let ret = unsafe { libc::ioctl(sock, libc::SIOCSIFFLAGS as _, &ifr) };
    unsafe { libc::close(sock) };

    if ret < 0 {
        return Err(format!("SIOCSIFFLAGS failed for {}", iface).into());
    }

    Ok(())
}

// ============================================================
// 3. Signal handlers (Linux only)
// ============================================================

#[cfg(target_os = "linux")]
fn install_signal_handlers() {
    // We do NOT set SIGCHLD=SIG_IGN here, because that breaks Command::output()
    // (waitpid returns ECHILD immediately when SIGCHLD=SIG_IGN).
    // Instead, reap_zombies() is called periodically with WNOHANG.
    log_info!("signal handlers installed");
}

// ============================================================
// 4. OCI config loading
// ============================================================

fn load_oci_config() -> Result<OciSpec, Box<dyn std::error::Error>> {
    for path in CONFIG_PATHS {
        if Path::new(path).exists() {
            log_info!("loading OCI config from {}", path);
            let data = std::fs::read_to_string(path)?;
            let spec: OciSpec = serde_json::from_str(&data)?;
            return Ok(spec);
        }
    }
    Err("no config.json found at any known path".into())
}

// ============================================================
// 5. Entrypoint execution
// ============================================================

#[cfg(target_os = "linux")]
fn run_entrypoint(process: OciProcess) {
    if process.args.is_empty() {
        log_error!("process.args is empty — no entrypoint to execute");
        reaper_loop();
    }

    let program = &process.args[0];
    let args = &process.args[1..];

    log_info!("executing entrypoint: {} {}", program, args.join(" "));

    // Set working directory
    if let Some(ref cwd) = process.cwd {
        if Path::new(cwd).exists() {
            if let Err(e) = std::env::set_current_dir(cwd) {
                log_error!("failed to chdir to '{}': {}", cwd, e);
            }
        }
    }

    // Set environment variables from OCI spec
    for env_entry in &process.env {
        if let Some((key, value)) = env_entry.split_once('=') {
            std::env::set_var(key, value);
        }
    }

    // Spawn the entrypoint. In initrd mode chroot into the virtiofs-mounted rootfs
    // first; in no-initrd mode we are already running inside the container rootfs.
    // PID 1 stays alive to continue reaping orphaned zombies.
    match unsafe {
        Command::new(program).args(args).pre_exec(|| {
            if is_initrd_mode() { chroot_into_rootfs() } else { Ok(()) }
        }).spawn()
    } {
        Ok(mut child) => {
            let child_pid = child.id();
            log_info!("entrypoint spawned (pid={})", child_pid);

            // Poll loop: wait for entrypoint while reaping zombies
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        let code = status.code().unwrap_or(1);
                        log_info!("entrypoint exited with code {}", code);
                        reap_zombies();
                        shutdown(code);
                        return;
                    }
                    Ok(None) => {
                        reap_zombies();
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                    Err(e) => {
                        log_error!("wait error: {}", e);
                        shutdown(1);
                        return;
                    }
                }
            }
        }
        Err(e) => {
            log_error!("failed to spawn entrypoint '{}': {}", program, e);
            reaper_loop();
        }
    }
}

// ============================================================
// 6. Zombie reaping
// ============================================================

/// Reap all finished child processes (non-blocking).
#[cfg(target_os = "linux")]
fn reap_zombies() {
    use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
    use nix::unistd::Pid;

    loop {
        match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => break,
            Ok(WaitStatus::Exited(pid, code)) => {
                log_info!("reaped zombie pid={} exit={}", pid, code);
            }
            Ok(WaitStatus::Signaled(pid, sig, _)) => {
                log_info!("reaped zombie pid={} signal={}", pid, sig);
            }
            Ok(_) => continue,
            Err(nix::errno::Errno::ECHILD) => break, // No children
            Err(_) => break,
        }
    }
}

/// Reaper-only loop — used when no entrypoint is configured.
/// PID 1 must **never** exit, so we loop forever.
#[cfg(target_os = "linux")]
fn reaper_loop() -> ! {
    log_info!("entering reaper-only mode (no entrypoint)");
    loop {
        reap_zombies();
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

// ============================================================
// 7. Graceful shutdown
// ============================================================

#[cfg(target_os = "linux")]
fn shutdown(exit_code: i32) {
    use std::ffi::CString;

    log_info!("shutting down (code={})", exit_code);

    // SIGTERM → all
    unsafe { libc::kill(-1, libc::SIGTERM) };
    std::thread::sleep(std::time::Duration::from_secs(2));

    // SIGKILL → all
    unsafe { libc::kill(-1, libc::SIGKILL) };
    reap_zombies();

    // Unmount pseudo-filesystems (best-effort, reverse order)
    let unmounts = [
        "/run", "/tmp", "/dev/shm", "/dev/pts", "/dev", "/sys", "/proc",
    ];
    for target in &unmounts {
        let c_target = match CString::new(*target) {
            Ok(c) => c,
            Err(_) => continue,
        };
        unsafe {
            libc::umount2(c_target.as_ptr(), libc::MNT_DETACH);
        }
    }

    // Sync filesystems
    unsafe { libc::sync() };

    // Power off the VM
    unsafe { libc::reboot(libc::LINUX_REBOOT_CMD_POWER_OFF) };

    // Unreachable, but satisfy the compiler
    std::process::exit(exit_code);
}
