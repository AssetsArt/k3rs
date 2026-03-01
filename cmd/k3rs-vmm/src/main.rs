//! k3rs-vmm — Host-side VMM helper for k3rs.
//!
//! Wraps Apple's Virtualization.framework to boot lightweight Linux microVMs
//! for container pod isolation on macOS. Rewritten in Rust using objc2-virtualization.

use std::path::Path;
use std::process;
use std::sync::Arc;
use std::thread;

use clap::{Parser, Subcommand};
use dispatch2::{MainThreadBound, dispatch_main};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::MainThreadMarker;
use objc2_virtualization::VZVirtualMachineDelegate;
use signal_hook::consts::signal::{SIGINT, SIGQUIT, SIGTERM};
use signal_hook::iterator::Signals;
use tracing::info;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

mod ipc;
mod linux_vm;
mod vm;
mod vm_delegate;
mod vsock;

/// VM configuration passed between modules.
#[derive(Debug, Clone)]
pub struct VmConfig {
    pub id: String,
    pub kernel_path: String,
    pub initrd_path: Option<String>,
    pub rootfs_path: String,
    pub cpu_count: usize,
    pub memory_mb: u64,
    pub log_path: Option<String>,
}

#[derive(Parser)]
#[command(name = "k3rs-vmm")]
#[command(
    about = "k3rs Virtual Machine Monitor — boots Linux microVMs via Virtualization.framework"
)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Boot a Linux microVM
    Boot(BootArgs),
    /// Stop a running microVM
    Stop(StopArgs),
    /// Execute a command inside a running microVM via vsock
    Exec(ExecArgs),
    /// Query the state of a microVM
    State(StateArgs),
    /// List running microVMs
    #[command(name = "ls")]
    List,
    /// Remove (kill) a running microVM
    #[command(name = "rm")]
    Rm(RmArgs),
}

// ── Boot ────────────────────────────────────────────────────────────────

#[derive(clap::Args)]
struct BootArgs {
    /// Path to Linux kernel (vmlinux)
    #[arg(long)]
    kernel: String,
    /// Path to initial ramdisk (initrd.img)
    #[arg(long)]
    initrd: Option<String>,
    /// Path to rootfs directory (shared via virtio-fs)
    #[arg(long)]
    rootfs: String,
    /// Number of vCPUs
    #[arg(long, default_value_t = 1)]
    cpus: usize,
    /// Memory in MB
    #[arg(long, default_value_t = 128)]
    memory: u64,
    /// Container/VM ID
    #[arg(long)]
    id: String,
    /// Path to log file for console output
    #[arg(long)]
    log: Option<String>,
    /// Run in foreground (block until VM exits)
    #[arg(long, default_value_t = false)]
    foreground: bool,
}

// ── Stop ────────────────────────────────────────────────────────────────

#[derive(clap::Args)]
struct StopArgs {
    /// Container/VM ID
    #[arg(long)]
    id: String,
}

// ── Exec ────────────────────────────────────────────────────────────────

#[derive(clap::Args)]
struct ExecArgs {
    /// Container/VM ID
    #[arg(long)]
    id: String,
    /// Allocate a PTY in the guest for interactive sessions (SSH-like)
    #[arg(long, default_value_t = false)]
    tty: bool,
    /// Command to execute in guest
    #[arg(trailing_var_arg = true)]
    command: Vec<String>,
}

// ── State ───────────────────────────────────────────────────────────────

#[derive(clap::Args)]
struct StateArgs {
    /// Container/VM ID
    #[arg(long)]
    id: String,
}

// ── Rm ──────────────────────────────────────────────────────────────────

#[derive(clap::Args)]
struct RmArgs {
    /// VM ID to remove
    id: String,
    /// Force kill (SIGKILL instead of SIGTERM)
    #[arg(short, long, default_value_t = false)]
    force: bool,
}

// ════════════════════════════════════════════════════════════════════════
// Main
// ════════════════════════════════════════════════════════════════════════

fn main() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .compact()
                .with_line_number(true)
                .with_thread_ids(true)
                .with_filter(LevelFilter::INFO),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Boot(args) => cmd_boot(args),
        Command::Stop(args) => cmd_stop(args),
        Command::Exec(args) => cmd_exec(args),
        Command::State(args) => cmd_state(args),
        Command::List => cmd_list(),
        Command::Rm(args) => cmd_rm(args),
    }
}

// ── Boot command ────────────────────────────────────────────────────────

fn cmd_boot(args: BootArgs) {
    info!(
        "booting VM: id={}, kernel={}, rootfs={}, cpus={}, memory={}MB",
        args.id, args.kernel, args.rootfs, args.cpus, args.memory
    );

    let config = VmConfig {
        id: args.id.clone(),
        kernel_path: args.kernel,
        initrd_path: args.initrd,
        rootfs_path: args.rootfs,
        cpu_count: args.cpus,
        memory_mb: args.memory,
        log_path: args.log,
    };

    // MainThreadMarker is required for Virtualization.framework
    let marker = MainThreadMarker::new().expect("must run on main thread");

    let vz_vm = linux_vm::create_vm(&config);

    // Set delegate
    let proto: Retained<ProtocolObject<dyn VZVirtualMachineDelegate>> =
        ProtocolObject::from_retained(vm_delegate::VmDelegate::new());
    unsafe {
        vz_vm.setDelegate(Some(&proto));
    }

    let vm = Arc::new(MainThreadBound::new(vz_vm, marker));
    vm::start_vm(Arc::clone(&vm));

    // Register VM ID for cleanup on exit (signal, delegate, start error)
    ipc::set_active_vm(&args.id);

    // Start IPC listener for exec requests.
    // Two closures: one for regular one-shot exec, one for streaming PTY exec.
    let id_for_ipc = args.id.clone();
    let vm_for_ipc = Arc::clone(&vm);
    let vm_for_ipc_stream = Arc::clone(&vm);
    ipc::start_listener(
        &id_for_ipc,
        move |parts| vsock::exec_via_vsock(&vm_for_ipc, parts),
        move |parts, ipc_stream| {
            vsock::exec_streaming_via_vsock(&vm_for_ipc_stream, parts, ipc_stream)
        },
    );

    // Handle signals for graceful shutdown
    let name = args.id.clone();
    let vm_for_signal = Arc::clone(&vm);
    let mut signals = Signals::new([SIGTERM, SIGINT, SIGQUIT]).unwrap();
    thread::spawn(move || {
        let signal = signals.forever().next().unwrap();
        info!(name, pid = process::id(), signal, "received signal");
        ipc::stop_listener(&name);
        vm::stop_vm(&name, vm_for_signal);
    });

    if !args.foreground {
        // Print info and keep running (daemon mode)
        println!("pid={}", process::id());
        println!("state=running");
    } else {
        info!(
            "VM {} running in foreground — press Ctrl+C to stop",
            args.id
        );
    }

    // Block on main thread — required for Virtualization.framework
    dispatch_main();
}

// ── Stop command ────────────────────────────────────────────────────────

fn cmd_stop(args: StopArgs) {
    info!("stopping VM: {}", args.id);

    // Find PID via pgrep
    let output = std::process::Command::new("pgrep")
        .args(["-f", &format!("k3rs-vmm boot.*--id {}", args.id)])
        .output();

    match output {
        Ok(o) if o.status.success() => {
            let pid_str = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if let Some(first_pid) = pid_str.lines().next() {
                if let Ok(pid) = first_pid.parse::<i32>() {
                    unsafe { libc::kill(pid, libc::SIGTERM) };
                    println!("state=stopped");
                    return;
                }
            }
        }
        _ => {}
    }

    eprintln!("VM {} not found", args.id);
    process::exit(1);
}

// ── Exec command ────────────────────────────────────────────────────────

fn cmd_exec(args: ExecArgs) {
    // Default to /bin/sh when no command is given (interactive shell).
    let command: Vec<String> = if args.command.is_empty() {
        vec!["/bin/sh".to_string()]
    } else {
        args.command.clone()
    };

    info!(
        "exec in VM {} tty={}: {}",
        args.id,
        args.tty,
        command.join(" ")
    );

    if args.tty {
        // Streaming PTY mode: bidirectional relay through IPC → vsock → guest PTY.
        match ipc::exec_streaming_via_ipc(&args.id, &command) {
            Ok(_) => {}
            Err(e) => {
                eprintln!("exec error: {}", e);
                process::exit(1);
            }
        }
    } else {
        // One-shot mode: collect output and print.
        match ipc::exec_via_ipc(&args.id, &command) {
            Ok(output) => print!("{}", output),
            Err(e) => {
                eprintln!("exec error: {}", e);
                process::exit(1);
            }
        }
    }
}

// ── State command ───────────────────────────────────────────────────────

fn cmd_state(args: StateArgs) {
    let sock_path = ipc::socket_path(&args.id);
    if Path::new(&sock_path).exists() {
        println!("state=running");
    } else {
        println!("state=not_found");
    }
}

// ── List command ────────────────────────────────────────────────────────

fn cmd_list() {
    let vms_dir = format!("{}/runtime/vms", pkg_constants::paths::DATA_DIR);

    let entries = match std::fs::read_dir(&vms_dir) {
        Ok(e) => e,
        Err(_) => {
            println!("No VMs found");
            return;
        }
    };

    let mut sockets: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|name| name.starts_with("vmm-") && name.ends_with(".sock"))
        .collect();
    sockets.sort();

    if sockets.is_empty() {
        println!("No VMs running");
        return;
    }

    println!("{:<38}  {:<8}  STATE", "VM ID", "PID");
    println!("{}", "-".repeat(60));

    for sock_file in &sockets {
        let id = sock_file
            .trim_start_matches("vmm-")
            .trim_end_matches(".sock");

        let mut pid = "-".to_string();
        let mut state = "running";

        // Find PID via pgrep
        let output = std::process::Command::new("pgrep")
            .args(["-f", &format!("k3rs-vmm boot.*--id {}", id)])
            .output();

        match output {
            Ok(o) if o.status.success() => {
                let pid_str = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if let Some(first) = pid_str.lines().next() {
                    if !first.is_empty() {
                        pid = first.to_string();
                    } else {
                        state = "stale";
                        let _ = std::fs::remove_file(format!("{}/{}", vms_dir, sock_file));
                    }
                } else {
                    state = "stale";
                    let _ = std::fs::remove_file(format!("{}/{}", vms_dir, sock_file));
                }
            }
            _ => {
                state = "stale";
                let _ = std::fs::remove_file(format!("{}/{}", vms_dir, sock_file));
            }
        }

        println!("{:<38}  {:<8}  {}", id, pid, state);
    }
}

// ── Rm command ──────────────────────────────────────────────────────────

fn cmd_rm(args: RmArgs) {
    let sock_path = ipc::socket_path(&args.id);

    // Find PID of the boot process
    let output = std::process::Command::new("pgrep")
        .args(["-f", &format!("k3rs-vmm boot.*--id {}", args.id)])
        .output();

    match output {
        Ok(o) if o.status.success() => {
            let pid_str = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if let Some(first_pid) = pid_str.lines().next() {
                if let Ok(pid) = first_pid.parse::<i32>() {
                    if args.force {
                        unsafe { libc::kill(pid, libc::SIGKILL) };
                        println!("VM {} force killed (pid={})", args.id, pid);
                    } else {
                        unsafe { libc::kill(pid, libc::SIGTERM) };
                        println!("VM {} stopped (pid={})", args.id, pid);

                        // Wait briefly, then force kill if still running
                        thread::sleep(std::time::Duration::from_secs(1));
                        if unsafe { libc::kill(pid, 0) } == 0 {
                            unsafe { libc::kill(pid, libc::SIGKILL) };
                            println!("VM {} force killed after timeout", args.id);
                        }
                    }
                } else {
                    println!("VM {} process not found", args.id);
                }
            } else {
                println!("VM {} process not found", args.id);
            }
        }
        _ => {
            println!("VM {} process not found", args.id);
        }
    }

    // Clean up socket file
    let _ = std::fs::remove_file(&sock_path);
    println!("Cleaned up {}", sock_path);
}
