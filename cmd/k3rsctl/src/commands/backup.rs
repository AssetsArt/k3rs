use crate::cli::BackupAction;

pub async fn handle_backup(
    client: &reqwest::Client,
    base: &str,
    action: &BackupAction,
) -> anyhow::Result<()> {
    match action {
        BackupAction::Create { output } => {
            let url = format!("{}/api/v1/cluster/backup", base);
            println!("Requesting backup from server...");
            let resp = client.post(&url).send().await?;
            if !resp.status().is_success() {
                eprintln!("Backup failed: server returned {}", resp.status());
                if let Ok(text) = resp.text().await {
                    eprintln!("  {}", text);
                }
                std::process::exit(1);
            }

            // Determine output filename from Content-Disposition or timestamp
            let filename = output.clone().unwrap_or_else(|| {
                resp.headers()
                    .get("content-disposition")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| {
                        s.split("filename=")
                            .nth(1)
                            .map(|f| f.trim_matches('"').to_string())
                    })
                    .unwrap_or_else(|| {
                        format!(
                            "backup-{}.k3rs-backup.json.gz",
                            chrono::Utc::now().format("%Y%m%d-%H%M%S")
                        )
                    })
            });

            let bytes = resp.bytes().await?;
            tokio::fs::write(&filename, &bytes).await?;
            println!("Backup saved to {} ({} bytes)", filename, bytes.len());
        }
        BackupAction::List { dir } => {
            let mut entries = tokio::fs::read_dir(dir).await?;
            let mut files: Vec<String> = Vec::new();
            while let Ok(Some(entry)) = entries.next_entry().await {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.ends_with(".k3rs-backup.json.gz") {
                    files.push(entry.path().to_string_lossy().to_string());
                }
            }
            files.sort();
            if files.is_empty() {
                println!("No backup files found.");
            } else {
                println!("{:<60} SIZE", "FILE");
                for f in &files {
                    let size = tokio::fs::metadata(f).await.map(|m| m.len()).unwrap_or(0);
                    println!("{:<60} {} bytes", f, size);
                }
            }
        }
        BackupAction::Inspect { file } => {
            let data = tokio::fs::read(file).await?;
            // Decompress and show metadata
            use std::io::Read as _;
            let mut decoder = flate2::read::GzDecoder::new(data.as_slice());
            let mut json = Vec::new();
            decoder.read_to_end(&mut json)?;
            let backup: serde_json::Value = serde_json::from_slice(&json)?;
            println!("Backup metadata:");
            println!(
                "  Version:      {}",
                backup["version"].as_str().unwrap_or("-")
            );
            println!(
                "  Created at:   {}",
                backup["created_at"].as_str().unwrap_or("-")
            );
            println!(
                "  Cluster:      {}",
                backup["cluster_name"].as_str().unwrap_or("-")
            );
            println!(
                "  Nodes:        {}",
                backup["node_count"].as_u64().unwrap_or(0)
            );
            println!(
                "  Entries:      {}",
                backup["key_count"].as_u64().unwrap_or(0)
            );
        }
        BackupAction::Status => {
            let url = format!("{}/api/v1/cluster/backup/status", base);
            let resp = client.get(&url).send().await?;
            if !resp.status().is_success() {
                eprintln!("Failed to get backup status: {}", resp.status());
                std::process::exit(1);
            }
            let status: serde_json::Value = resp.json().await?;
            println!(
                "Backup status:  {}",
                status["status"].as_str().unwrap_or("-")
            );
            println!(
                "Last backup:    {}",
                status["last_backup_at"].as_str().unwrap_or("(never)")
            );
            println!(
                "Last file:      {}",
                status["last_backup_file"].as_str().unwrap_or("-")
            );
            println!(
                "Keys backed up: {}",
                status["key_count"].as_u64().unwrap_or(0)
            );
        }
    }
    Ok(())
}

pub async fn handle_restore(
    client: &reqwest::Client,
    base: &str,
    from: &str,
    dry_run: bool,
    force: bool,
) -> anyhow::Result<()> {
    let url = if dry_run {
        format!("{}/api/v1/cluster/restore/dry-run", base)
    } else {
        format!("{}/api/v1/cluster/restore", base)
    };

    // Read backup file
    let data = match tokio::fs::read(from).await {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Failed to read backup file '{}': {}", from, e);
            std::process::exit(1);
        }
    };

    if dry_run {
        println!("Running dry-run validation of backup '{}'...", from);
    } else {
        if !force {
            eprintln!(
                "WARNING: This will WIPE all cluster data and restore from '{}'.",
                from
            );
            eprintln!("Use --force to confirm, or --dry-run to validate only.");
            std::process::exit(1);
        }
        println!("Restoring cluster from '{}'...", from);
    }

    let resp = client
        .post(&url)
        .header("Content-Type", "application/octet-stream")
        .body(data)
        .send()
        .await?;

    let status = resp.status();
    let result: serde_json::Value = resp.json().await?;

    if status.is_success() {
        if dry_run {
            println!("Dry-run validation passed:");
            println!(
                "  Entries to restore: {}",
                result["would_restore"].as_u64().unwrap_or(0)
            );
            println!(
                "  Backup created at:  {}",
                result["backup_created_at"].as_str().unwrap_or("-")
            );
            println!(
                "  Backup version:     {}",
                result["backup_version"].as_str().unwrap_or("-")
            );
        } else {
            println!("Restore completed:");
            println!(
                "  Entries imported:   {}",
                result["imported"].as_u64().unwrap_or(0)
            );
            println!(
                "  Backup created at:  {}",
                result["backup_created_at"].as_str().unwrap_or("-")
            );
        }
    } else {
        eprintln!(
            "Restore failed ({}): {}",
            status,
            result["error"].as_str().unwrap_or("unknown error")
        );
        std::process::exit(1);
    }
    Ok(())
}
