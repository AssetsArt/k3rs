use crate::cli::RuntimeAction;

pub async fn handle(
    client: &reqwest::Client,
    server: &str,
    action: &RuntimeAction,
) -> anyhow::Result<()> {
    match action {
        RuntimeAction::Info => {
            let resp = client
                .get(format!("{}/api/v1/runtime", server))
                .send()
                .await?;
            let info: serde_json::Value = resp.json().await?;
            println!("Container Runtime");
            println!(
                "  Backend:  {}",
                info["backend"].as_str().unwrap_or("unknown")
            );
            println!(
                "  Version:  {}",
                info["version"].as_str().unwrap_or("unknown")
            );
            println!("  OS:       {}", info["os"].as_str().unwrap_or("unknown"));
            println!("  Arch:     {}", info["arch"].as_str().unwrap_or("unknown"));
        }
        RuntimeAction::Upgrade => {
            println!("Upgrading container runtime...");
            let resp = client
                .put(format!("{}/api/v1/runtime/upgrade", server))
                .send()
                .await?;
            let result: serde_json::Value = resp.json().await?;
            println!("Status: {}", result["status"].as_str().unwrap_or("unknown"));
            if let Some(msg) = result["message"].as_str() {
                println!("Message: {}", msg);
            }
        }
        RuntimeAction::KernelDownload { data_dir } => {
            kernel_download(client, data_dir.as_deref()).await?;
        }
    }
    Ok(())
}

/// Download vmlinux + initrd.img from their respective GitHub releases.
///
/// Kernel assets are published as separate releases:
///   - `kernel-v*` releases contain `vmlinux-{arch}`
///   - `initrd-v*` releases contain `initrd.img-{arch}`
async fn kernel_download(_client: &reqwest::Client, data_dir: Option<&str>) -> anyhow::Result<()> {
    let dest_dir = data_dir.unwrap_or(pkg_constants::paths::DATA_DIR);
    let repo = pkg_constants::network::GITHUB_REPO;
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "amd64",
        other => other,
    };

    println!("Fetching releases from github.com/{}...", repo);

    // Use a plain client without the k3rs auth token for GitHub API calls.
    let github = reqwest::Client::new();

    let resp = github
        .get(format!("https://api.github.com/repos/{}/releases", repo))
        .header("User-Agent", "k3rsctl")
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!(
            "GitHub API returned HTTP {status}.\n\
             This is likely a rate limit — try again in a minute or set GITHUB_TOKEN.\n\
             Response: {body}"
        );
    }

    let releases: Vec<serde_json::Value> = resp.json().await?;

    // Create destination directory
    std::fs::create_dir_all(dest_dir)?;

    // 1. Download vmlinux from the latest kernel-v* release
    let kernel_tag = find_release_tag(&releases, "kernel-v").ok_or_else(|| {
        anyhow::anyhow!(
            "No kernel-v* release found in github.com/{}.\n\
                 Build from source instead: ./scripts/build-kernel.sh",
            repo
        )
    })?;

    println!("Found kernel release: {}", kernel_tag);

    let vmlinux_name = format!("vmlinux-{}", arch);
    let vmlinux_url = format!(
        "https://github.com/{}/releases/download/{}/{}",
        repo, kernel_tag, vmlinux_name
    );
    let vmlinux_dest = format!("{}/{}", dest_dir, pkg_constants::vm::KERNEL_FILENAME);

    println!("Downloading {} -> {}...", vmlinux_name, vmlinux_dest);
    download_file(&github, &vmlinux_url, &vmlinux_dest).await?;

    // 2. Download initrd.img from the latest initrd-v* release
    let initrd_tag = find_release_tag(&releases, "initrd-v").ok_or_else(|| {
        anyhow::anyhow!(
            "No initrd-v* release found in github.com/{}.\n\
                 Build from source instead: ./scripts/build-kernel.sh",
            repo
        )
    })?;

    println!("Found initrd release: {}", initrd_tag);

    let initrd_name = format!("initrd.img-{}", arch);
    let initrd_url = format!(
        "https://github.com/{}/releases/download/{}/{}",
        repo, initrd_tag, initrd_name
    );
    let initrd_dest = format!("{}/{}", dest_dir, pkg_constants::vm::INITRD_FILENAME);

    println!("Downloading {} -> {}...", initrd_name, initrd_dest);
    download_file(&github, &initrd_url, &initrd_dest).await?;

    println!();
    println!("Kernel assets installed to {}:", dest_dir);
    println!(
        "  {} ({})",
        pkg_constants::vm::KERNEL_FILENAME,
        file_size(&vmlinux_dest)
    );
    println!(
        "  {} ({})",
        pkg_constants::vm::INITRD_FILENAME,
        file_size(&initrd_dest)
    );

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

/// Download a file from a URL, following redirects.
async fn download_file(client: &reqwest::Client, url: &str, dest: &str) -> anyhow::Result<()> {
    let resp = client
        .get(url)
        .header("User-Agent", "k3rsctl")
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!("Download failed: HTTP {} for {}", resp.status(), url);
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

fn file_size(path: &str) -> String {
    match std::fs::metadata(path) {
        Ok(m) => {
            let bytes = m.len();
            if bytes >= 1_000_000 {
                format!("{:.1} MB", bytes as f64 / 1_048_576.0)
            } else if bytes >= 1_000 {
                format!("{:.0} KB", bytes as f64 / 1_024.0)
            } else {
                format!("{} B", bytes)
            }
        }
        Err(_) => "unknown".to_string(),
    }
}
