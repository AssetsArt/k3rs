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
fn fixed(msg: &str) {
    println!("  {GREEN}[FIXED]{RESET} {msg}");
}

pub async fn handle(client: &reqwest::Client, _base: &str, fix: bool) -> anyhow::Result<()> {
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
                    warn(&format!(
                        "'{tool}' not found — install libcap2-bin (Debian/Ubuntu) or libcap (Fedora)"
                    ));
                    warns += 1;
                }
                "wg" => {
                    warn(&format!(
                        "'{tool}' not found — cross-node traffic disabled without wireguard-tools"
                    ));
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
    } else if fix {
        println!("  Installing cargo-watch...");
        let status = Command::new("cargo")
            .args(["install", "cargo-watch"])
            .status();
        if status.map(|s| s.success()).unwrap_or(false) {
            fixed("cargo-watch installed");
            passes += 1;
        } else {
            fail("Failed to install cargo-watch");
            fails += 1;
        }
    } else {
        warn("'cargo-watch' not found (cargo install cargo-watch)");
        warns += 1;
    }
    println!();

    // ── Kernel Assets ────────────────────────────────────────────────
    println!("{BOLD}Kernel Assets{RESET}");

    let data_dir = pkg_constants::paths::DATA_DIR;
    let kernel_path = format!("{}/{}", data_dir, pkg_constants::vm::KERNEL_FILENAME);
    let initrd_path = format!("{}/{}", data_dir, pkg_constants::vm::INITRD_FILENAME);

    let kernel_missing = !Path::new(&kernel_path).exists();
    let initrd_missing = !Path::new(&initrd_path).exists();

    if kernel_missing || initrd_missing {
        if fix {
            println!("  Downloading kernel assets...");
            match download_kernel(client, data_dir).await {
                Ok(()) => {
                    // Re-check after download
                    if Path::new(&kernel_path).exists() {
                        let size = std::fs::metadata(&kernel_path)
                            .map(|m| format_size(m.len()))
                            .unwrap_or_default();
                        fixed(&format!("vmlinux downloaded ({size})"));
                        passes += 1;
                    } else {
                        fail("vmlinux download failed");
                        fails += 1;
                        need_kernel = true;
                    }
                    if Path::new(&initrd_path).exists() {
                        let size = std::fs::metadata(&initrd_path)
                            .map(|m| format_size(m.len()))
                            .unwrap_or_default();
                        fixed(&format!("initrd.img downloaded ({size})"));
                        passes += 1;
                    } else {
                        fail("initrd.img download failed");
                        fails += 1;
                        need_kernel = true;
                    }
                }
                Err(e) => {
                    fail(&format!("Download failed: {e}"));
                    fails += 1;
                    need_kernel = true;
                }
            }
        } else {
            if kernel_missing {
                fail(&format!("vmlinux not found at {kernel_path}"));
                fails += 1;
                need_kernel = true;
            } else {
                let size = std::fs::metadata(&kernel_path)
                    .map(|m| format_size(m.len()))
                    .unwrap_or_default();
                pass(&format!("vmlinux ({size})"));
                passes += 1;
            }
            if initrd_missing {
                fail(&format!("initrd.img not found at {initrd_path}"));
                fails += 1;
                need_kernel = true;
            } else {
                let size = std::fs::metadata(&initrd_path)
                    .map(|m| format_size(m.len()))
                    .unwrap_or_default();
                pass(&format!("initrd.img ({size})"));
                passes += 1;
            }
        }
    } else {
        let ksize = std::fs::metadata(&kernel_path)
            .map(|m| format_size(m.len()))
            .unwrap_or_default();
        let isize = std::fs::metadata(&initrd_path)
            .map(|m| format_size(m.len()))
            .unwrap_or_default();
        pass(&format!("vmlinux ({ksize})"));
        pass(&format!("initrd.img ({isize})"));
        passes += 2;
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
        let cap_bins: &[(&str, &str)] = &[("agent", "k3rs-agent"), ("vpc", "k3rs-vpc")];

        for &(key, bin_name) in cap_bins {
            let debug_bin = format!("target/debug/{}", bin_name);
            let required = pkg_constants::capabilities::caps_for_component(key);
            if required.is_empty() {
                continue;
            }

            if !Path::new(&debug_bin).exists() {
                // Not built yet — skip, k3rs-dev will handle it
                continue;
            }

            if has_all_caps(&debug_bin, required) {
                pass(&format!("{debug_bin}: OK"));
                passes += 1;
            } else if fix {
                // Run sudo setcap
                let cap_str = required
                    .iter()
                    .map(|c| c.to_lowercase())
                    .collect::<Vec<_>>()
                    .join(",")
                    + "+eip";

                println!("  Setting capabilities on {debug_bin}...");
                let status = Command::new("sudo")
                    .args(["setcap", &cap_str, &debug_bin])
                    .status();

                if status.map(|s| s.success()).unwrap_or(false) {
                    fixed(&format!("{debug_bin}: {cap_str}"));
                    passes += 1;
                } else {
                    fail(&format!("{debug_bin}: sudo setcap failed"));
                    fails += 1;
                    need_caps = true;
                }
            } else {
                let missing: Vec<&str> = required
                    .iter()
                    .filter(|cap| !has_cap(&debug_bin, cap))
                    .copied()
                    .collect();
                warn(&format!("{debug_bin}: missing {}", missing.join(", ")));
                warns += 1;
                need_caps = true;
            }
        }
    }
    println!();

    // ── Summary ──────────────────────────────────────────────────────
    println!("{BOLD}Summary{RESET}");
    println!(
        "  {GREEN}{passes} passed{RESET}, {YELLOW}{warns} warning(s){RESET}, {RED}{fails} failed{RESET}"
    );

    let has_fixable = need_caps || need_kernel || need_tools;

    if has_fixable && !fix {
        println!();
        println!(
            "  Run {BOLD}k3rsctl doctor --fix{RESET} to auto-fix capabilities and download kernel."
        );
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
                    if has_apt {
                        "iproute2"
                    } else {
                        "iproute"
                    }
                }
                "setcap" | "getcap" => {
                    if has_apt {
                        "libcap2-bin"
                    } else {
                        "libcap"
                    }
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
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(name).is_file()))
        .unwrap_or(false)
}

/// Check if a binary has a specific capability set.
fn has_cap(bin: &str, cap: &str) -> bool {
    Command::new("getcap")
        .arg(bin)
        .output()
        .map(|o| {
            o.status.success()
                && String::from_utf8_lossy(&o.stdout)
                    .to_lowercase()
                    .contains(&cap.to_lowercase())
        })
        .unwrap_or(false)
}

/// Check if a binary has all required capabilities.
fn has_all_caps(bin: &str, required: &[&str]) -> bool {
    let output = Command::new("getcap").arg(bin).output();
    match output {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout).to_lowercase();
            required
                .iter()
                .all(|cap| stdout.contains(&cap.to_lowercase()))
        }
        _ => false,
    }
}

/// Download vmlinux + initrd.img from their respective GitHub releases.
///
/// Kernel assets are published as separate releases:
///   - `kernel-v*` releases contain `vmlinux-{arch}`
///   - `initrd-v*` releases contain `initrd.img-{arch}`
async fn download_kernel(_client: &reqwest::Client, dest_dir: &str) -> anyhow::Result<()> {
    let repo = pkg_constants::network::GITHUB_REPO;
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "amd64",
        other => other,
    };

    // Use a plain client without the k3rs auth token for GitHub API calls.
    let github = reqwest::Client::new();

    let resp = github
        .get(format!("https://api.github.com/repos/{}/releases", repo))
        .header("User-Agent", "k3rsctl")
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!(
            "GitHub API returned HTTP {} (rate limit? try again or set GITHUB_TOKEN)",
            resp.status()
        );
    }

    let releases: Vec<serde_json::Value> = resp.json().await?;

    std::fs::create_dir_all(dest_dir)?;

    // Download vmlinux from kernel-v* release
    let kernel_tag = find_release_tag(&releases, "kernel-v")
        .ok_or_else(|| anyhow::anyhow!("No kernel-v* release found"))?;

    download_asset(
        &github,
        repo,
        &kernel_tag,
        &format!("vmlinux-{}", arch),
        &format!("{}/{}", dest_dir, pkg_constants::vm::KERNEL_FILENAME),
    )
    .await?;

    // Download initrd.img from initrd-v* release
    let initrd_tag = find_release_tag(&releases, "initrd-v")
        .ok_or_else(|| anyhow::anyhow!("No initrd-v* release found"))?;

    download_asset(
        &github,
        repo,
        &initrd_tag,
        &format!("initrd.img-{}", arch),
        &format!("{}/{}", dest_dir, pkg_constants::vm::INITRD_FILENAME),
    )
    .await?;

    Ok(())
}

/// Find the latest release tag matching a given prefix.
fn find_release_tag(releases: &[serde_json::Value], prefix: &str) -> Option<String> {
    releases.iter().find_map(|r| {
        r["tag_name"]
            .as_str()
            .filter(|t| t.starts_with(prefix))
            .map(|t| t.to_string())
    })
}

/// Download a single asset from a GitHub release.
async fn download_asset(
    client: &reqwest::Client,
    repo: &str,
    tag: &str,
    asset: &str,
    dest: &str,
) -> anyhow::Result<()> {
    let url = format!(
        "https://github.com/{}/releases/download/{}/{}",
        repo, tag, asset
    );

    let resp = client
        .get(&url)
        .header("User-Agent", "k3rsctl")
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!("HTTP {} for {}", resp.status(), url);
    }

    let bytes = resp.bytes().await?;
    std::fs::write(dest, &bytes)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755))?;
    }

    Ok(())
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
