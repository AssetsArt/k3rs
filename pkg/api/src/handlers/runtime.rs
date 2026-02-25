use axum::{Json, extract::State, response::IntoResponse};
use tracing::info;

use crate::AppState;

/// GET /api/v1/runtime — returns current runtime info.
pub async fn get_runtime_info(State(state): State<AppState>) -> impl IntoResponse {
    let runtime = &state.container_runtime;
    let info = runtime.runtime_info();

    Json(serde_json::json!({
        "backend": info.backend,
        "version": info.version,
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
    }))
}

/// PUT /api/v1/runtime/upgrade — trigger auto-download of the latest runtime (Linux only).
pub async fn upgrade_runtime(State(_state): State<AppState>) -> impl IntoResponse {
    info!("Runtime upgrade requested");

    if cfg!(target_os = "macos") {
        return Json(serde_json::json!({
            "status": "skipped",
            "message": "Runtime upgrade not supported on macOS (using Docker)",
        }));
    }

    match pkg_container::installer::RuntimeInstaller::ensure_runtime(None).await {
        Ok(path) => Json(serde_json::json!({
            "status": "success",
            "runtime_path": path.to_string_lossy(),
            "message": "Runtime downloaded/verified successfully",
        })),
        Err(e) => Json(serde_json::json!({
            "status": "error",
            "message": format!("Failed to upgrade runtime: {}", e),
        })),
    }
}
