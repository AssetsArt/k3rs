use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use chrono::Utc;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use pkg_types::backup::{BACKUP_VERSION, BackupEntry, BackupFile, BackupPki, BackupStatus};
use std::io::{Read, Write};
use std::sync::atomic::Ordering;
use tracing::{info, warn};

use crate::AppState;

// ─────────────────────────────────────────────────────────────────────────────
// Core backup / restore helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Snapshot the store and produce a `BackupFile` + raw gzip bytes.
pub async fn create_backup_bytes(state: &AppState) -> anyhow::Result<(BackupFile, Vec<u8>)> {
    let entries_raw = state.store.snapshot().await?;

    let node_count = entries_raw
        .iter()
        .filter(|(k, _)| k.starts_with("/registry/nodes/"))
        .count();

    let entries: Vec<BackupEntry> = entries_raw
        .into_iter()
        .filter_map(|(key, value)| {
            let v = serde_json::from_slice(&value).ok()?;
            Some(BackupEntry { key, value: v })
        })
        .collect();

    let key_count = entries.len();

    let backup = BackupFile {
        version: BACKUP_VERSION.to_string(),
        created_at: Utc::now(),
        cluster_name: "k3rs".to_string(),
        node_count,
        key_count,
        entries,
        pki: BackupPki {
            ca_cert: state.ca.ca_cert_pem().to_string(),
        },
    };

    let json = serde_json::to_vec_pretty(&backup)?;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&json)?;
    let compressed = encoder.finish()?;

    Ok((backup, compressed))
}

/// Validate a `BackupFile`: check version and non-empty entries.
pub fn validate_backup(backup: &BackupFile) -> anyhow::Result<()> {
    if backup.version != BACKUP_VERSION {
        anyhow::bail!(
            "Unsupported backup version '{}' (expected '{}')",
            backup.version,
            BACKUP_VERSION
        );
    }
    if backup.entries.is_empty() {
        anyhow::bail!("Backup contains no entries");
    }
    Ok(())
}

/// Decompress + deserialize gzip backup bytes into a `BackupFile`.
pub fn parse_backup_bytes(data: &[u8]) -> anyhow::Result<BackupFile> {
    let mut decoder = GzDecoder::new(data);
    let mut json = Vec::new();
    decoder.read_to_end(&mut json)?;
    let backup: BackupFile = serde_json::from_slice(&json)?;
    Ok(backup)
}

// ─────────────────────────────────────────────────────────────────────────────
// HTTP handlers
// ─────────────────────────────────────────────────────────────────────────────

/// `GET /api/v1/cluster/backup/status`
/// Returns metadata about the most recent backup.
pub async fn backup_status(State(state): State<AppState>) -> impl IntoResponse {
    let last = state
        .store
        .get("/registry/_backup/last")
        .await
        .ok()
        .flatten();

    let status = if let Some(data) = last {
        serde_json::from_slice::<BackupStatus>(&data).unwrap_or(BackupStatus {
            last_backup_at: None,
            last_backup_file: None,
            key_count: None,
            status: "unknown".to_string(),
        })
    } else {
        BackupStatus {
            last_backup_at: None,
            last_backup_file: None,
            key_count: None,
            status: "no_backup".to_string(),
        }
    };

    (StatusCode::OK, Json(status))
}

/// `POST /api/v1/cluster/backup`
/// Snapshot the cluster state and stream the gzip backup as a download.
pub async fn create_backup_handler(State(state): State<AppState>) -> axum::response::Response {
    if state.restore_in_progress.load(Ordering::SeqCst) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "restore in progress"})),
        )
            .into_response();
    }

    match create_backup_bytes(&state).await {
        Ok((backup, compressed)) => {
            let filename = format!(
                "backup-{}.k3rs-backup.json.gz",
                backup.created_at.format("%Y%m%d-%H%M%S")
            );
            info!(
                "Backup created on-demand: {} keys → {}",
                backup.key_count, filename
            );

            // Persist last-backup metadata to store
            let st = BackupStatus {
                last_backup_at: Some(backup.created_at),
                last_backup_file: Some(filename.clone()),
                key_count: Some(backup.key_count),
                status: "success".to_string(),
            };
            if let Ok(data) = serde_json::to_vec(&st) {
                let _ = state.store.put("/registry/_backup/last", &data).await;
            }

            let mut headers = axum::http::HeaderMap::new();
            headers.insert(
                axum::http::header::CONTENT_TYPE,
                "application/gzip".parse().unwrap(),
            );
            headers.insert(
                axum::http::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}\"", filename)
                    .parse()
                    .unwrap(),
            );
            headers.insert(
                axum::http::header::CONTENT_LENGTH,
                compressed.len().to_string().parse().unwrap(),
            );

            (StatusCode::OK, headers, compressed).into_response()
        }
        Err(e) => {
            warn!("On-demand backup failed: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response()
        }
    }
}

/// `POST /api/v1/cluster/restore`
/// Upload a `.k3rs-backup.json.gz` to restore the cluster.
/// **Leader-only** — returns 403 if this server is not the leader.
pub async fn restore_cluster_handler(
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> (StatusCode, Json<serde_json::Value>) {
    if !state.is_leader.load(Ordering::SeqCst) {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "restore can only be triggered on the leader"})),
        );
    }
    do_restore(&state, &body, false).await
}

/// `POST /api/v1/cluster/restore/dry-run`
/// Parse + validate a backup file without applying any changes.
pub async fn restore_dry_run_handler(
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> (StatusCode, Json<serde_json::Value>) {
    do_restore(&state, &body, true).await
}

// ─────────────────────────────────────────────────────────────────────────────
// Shared restore implementation
// ─────────────────────────────────────────────────────────────────────────────

async fn do_restore(
    state: &AppState,
    data: &[u8],
    dry_run: bool,
) -> (StatusCode, Json<serde_json::Value>) {
    // Parse
    let backup = match parse_backup_bytes(data) {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("Failed to parse backup: {}", e)})),
            );
        }
    };

    // Validate
    if let Err(e) = validate_backup(&backup) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": format!("Invalid backup: {}", e)})),
        );
    }

    // Dry-run: return info without touching the store
    if dry_run {
        info!(
            "Restore dry-run OK: {} entries, backup created at {}",
            backup.key_count, backup.created_at
        );
        return (
            StatusCode::OK,
            Json(serde_json::json!({
                "dry_run": true,
                "would_restore": backup.key_count,
                "backup_created_at": backup.created_at,
                "backup_version": backup.version,
                "cluster_name": backup.cluster_name,
            })),
        );
    }

    // ── Live restore ─────────────────────────────────────────────────────────

    // Signal "restore in progress" → all other requests get 503
    state.restore_in_progress.store(true, Ordering::SeqCst);
    let _ = state
        .store
        .put("/registry/_restore/status", b"in_progress")
        .await;

    info!(
        "Starting cluster restore: {} entries from backup {}",
        backup.key_count, backup.created_at
    );

    let result = perform_restore(state, &backup).await;

    // Clear restore flag regardless of outcome
    state.restore_in_progress.store(false, Ordering::SeqCst);

    match result {
        Ok(imported) => {
            // Bump restore epoch so followers can detect the change
            let epoch = Utc::now().timestamp();
            let _ = state
                .store
                .put("/registry/_restore/epoch", epoch.to_string().as_bytes())
                .await;
            let _ = state
                .store
                .put("/registry/_restore/status", b"completed")
                .await;

            info!("Cluster restore completed: {} entries imported", imported);
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "status": "completed",
                    "imported": imported,
                    "backup_created_at": backup.created_at,
                })),
            )
        }
        Err(e) => {
            warn!("Cluster restore failed: {}", e);
            let _ = state
                .store
                .put("/registry/_restore/status", b"failed")
                .await;
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Restore failed: {}", e)})),
            )
        }
    }
}

/// Wipe existing registry keys and import all backup entries.
async fn perform_restore(state: &AppState, backup: &BackupFile) -> anyhow::Result<usize> {
    // Wipe all registry keys (preserve _restore/ and _backup/ metadata)
    let existing = state.store.snapshot().await?;
    for (key, _) in &existing {
        if !key.starts_with("/registry/_restore/") && !key.starts_with("/registry/_backup/") {
            state.store.delete(key).await?;
        }
    }

    // Import backup entries
    let mut imported = 0usize;
    for entry in &backup.entries {
        let value = serde_json::to_vec(&entry.value)?;
        state.store.put(&entry.key, &value).await?;
        imported += 1;
    }

    Ok(imported)
}

// ─────────────────────────────────────────────────────────────────────────────
// Restore-guard middleware
// ─────────────────────────────────────────────────────────────────────────────

/// Axum middleware that returns 503 for all non-restore routes while a
/// cluster restore is in progress.
pub async fn restore_guard_middleware(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let path = request.uri().path().to_string();
    if state.restore_in_progress.load(Ordering::SeqCst) && !path.contains("/restore") {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "cluster restore in progress"})),
        )
            .into_response();
    }
    next.run(request).await
}
