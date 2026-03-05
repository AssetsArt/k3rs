use std::path::Path;
use std::process::Command;

const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

fn pass(msg: &str) {
    println!("  {GREEN}[PASS]{RESET} {msg}");
}
fn warn(msg: &str) {
    println!("  {YELLOW}[WARN]{RESET} {msg}");
}
fn fail(msg: &str) {
    println!("  {RED}[FAIL]{RESET} {msg}");
}

pub async fn handle(_client: &reqwest::Client, _base: &str) -> anyhow::Result<()> {
    let mut passes = 0u32;
    let mut warns = 0u32;
    let mut fails = 0u32;

    let mut need_caps = false;
    let mut need_kernel = false;
    let mut need_tools = false;

    // ── CLI Tools ────────────────────────────────────────────────────
    println!("{BOLD}CLI Tools{RESET}");

    let mut missing_tools: Vec<&str> = Vec::new();
    for tool in &["ip", "nsenter", "wg", "setcap", "getcap"] {
        if which(tool) {
            pass(&format!("'{tool}' found"));
            passes += 1;
        } else {
            missing_tools.push(tool);
            need_tools = true;
            match *tool {
                "setcap" | "getcap" => {
                    warn(&format!("'{tool}' not found — install libcap2-bin (Debian/Ubuntu) or libcap (Fedora)"));
                    warns += 1;
                }
                "wg" => {
                    warn(&format!("'{tool}' not found — cross-node traffic disabled without wireguard-tools"));
                    warns += 1;
                }
                _ => {
                    fail(&format!("'{tool}' not found — required for pod networking"));
                    fails += 1;
                }
            }
        }
    }

    // cargo-watch (needed for k3rs-dev)
    let has_cargo_watch = Command::new("cargo")
        .args(["watch", "--version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if has_cargo_watch {
        pass("'cargo-watch' found");
        passes += 1;
    } else {
        warn("'cargo-watch' not found — needed for k3rs-dev (cargo install cargo-watch)");
        warns += 1;
    }
    println!();

    // ── Kernel Assets ────────────────────────────────────────────────
    println!("{BOLD}Kernel Assets{RESET}");

    let data_dir = pkg_constants::paths::DATA_DIR;
    let kernel_path = format!("{}/{}", data_dir, pkg_constants::vm::KERNEL_FILENAME);
    let initrd_path = format!("{}/{}", data_dir, pkg_constants::vm::INITRD_FILENAME);

    if Path::new(&kernel_path).exists() {
        let size = std::fs::metadata(&kernel_path)
            .map(|m| format_size(m.len()))
            .unwrap_or_default();
        pass(&format!("vmlinux ({size})"));
        passes += 1;
    } else {
        fail(&format!("vmlinux not found at {kernel_path}"));
        fails += 1;
        need_kernel = true;
    }

    if Path::new(&initrd_path).exists() {
        let size = std::fs::metadata(&initrd_path)
            .map(|m| format_size(m.len()))
            .unwrap_or_default();
        pass(&format!("initrd.img ({size})"));
        passes += 1;
    } else {
        fail(&format!("initrd.img not found at {initrd_path}"));
        fails += 1;
        need_kernel = true;
    }
    println!();

    // ── Capabilities ─────────────────────────────────────────────────
    println!("{BOLD}Capabilities{RESET}");

    let is_root = Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
        .unwrap_or(false);

    if is_root {
        pass("Running as root");
        passes += 1;
    } else {
        // Check caps on target/debug binaries (dev workflow)
        for (key, bin_name) in &[("agent", "k3rs-agent"), ("vpc", "k3rs-vpc")] {
            let debug_bin = format!("target/debug/{}", bin_name);
            if Path::new(&debug_bin).exists() {
                let required = pkg_constants::capabilities::caps_for_component(key);
                check_file_caps(
                    Path::new(&debug_bin),
                    required,
                    &mut passes,
                    &mut warns,
                    &mut need_caps,
                );
            }
            // not built yet — k3rs-dev will handle it
        }

        if !need_caps {
            pass("Capabilities set on debug binaries");
            passes += 1;
        }
    }
    println!();

    // ── Summary ──────────────────────────────────────────────────────
    println!("{BOLD}Summary{RESET}");
    println!(
        "  {GREEN}{passes} passed{RESET}, {YELLOW}{warns} warning(s){RESET}, {RED}{fails} failed{RESET}"
    );

    if need_caps || need_kernel || need_tools {
        println!();
        println!("  {BOLD}Quick fixes:{RESET}");
    }

    if need_caps {
        println!();
        println!("  {BOLD}Capabilities:{RESET}");
        println!("    k3rs-dev all              # auto-builds and runs sudo setcap");
    }

    if need_kernel {
        println!();
        println!("  {BOLD}Kernel assets:{RESET}");
        println!("    k3rsctl runtime kernel-download");
        println!("    or: ./scripts/build-kernel.sh");
    }

    if need_tools {
        println!();
        println!("  {BOLD}Missing tools:{RESET}");
        let has_apt = which("apt");
        let has_dnf = which("dnf");
        let has_pacman = which("pacman");
        let pkgs: Vec<&str> = missing_tools
            .iter()
            .map(|t| match *t {
                "ip" | "nsenter" => {
                    if has_apt { "iproute2" } else { "iproute" }
                }
                "setcap" | "getcap" => {
                    if has_apt { "libcap2-bin" } else { "libcap" }
                }
                "wg" => "wireguard-tools",
                other => other,
            })
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        if has_apt {
            println!("    sudo apt install {}", pkgs.join(" "));
        } else if has_dnf {
            println!("    sudo dnf install {}", pkgs.join(" "));
        } else if has_pacman {
            println!("    sudo pacman -S {}", pkgs.join(" "));
        } else {
            println!("    Install: {}", pkgs.join(" "));
        }
    }

    if fails > 0 {
        println!();
        std::process::exit(1);
    } else if warns > 0 {
        println!();
        println!("  Some warnings — cluster may still work.");
    } else {
        println!();
        println!("  {GREEN}Ready to run!{RESET} Start with: k3rs-dev all");
    }

    Ok(())
}

fn which(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| dir.join(name).is_file())
        })
        .unwrap_or(false)
}

fn check_file_caps(bin: &Path, required: &[&str], passes: &mut u32, warns: &mut u32, need_caps: &mut bool) {
    if required.is_empty() {
        return;
    }

    let bin_display = bin.display();
    let output = Command::new("getcap").arg(bin).output();

    match output {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout).to_lowercase();
            let missing: Vec<&&str> = required
                .iter()
                .filter(|cap| !stdout.contains(&cap.to_lowercase()))
                .collect();

            if missing.is_empty() {
                pass(&format!("{bin_display}: OK"));
                *passes += 1;
            } else {
                warn(&format!(
                    "{bin_display}: missing {}",
                    missing.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(", ")
                ));
                *warns += 1;
                *need_caps = true;
            }
        }
        _ => {
            warn(&format!("{bin_display}: cannot verify (getcap failed)"));
            *warns += 1;
            *need_caps = true;
        }
    }
}

fn format_size(bytes: u64) -> String {
    if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1_000 {
        format!("{:.0} KB", bytes as f64 / 1_024.0)
    } else {
        format!("{} B", bytes)
    }
}
