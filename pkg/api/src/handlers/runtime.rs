use axum::{Json, http::StatusCode, response::IntoResponse};
use tracing::info;

/// GET /api/v1/runtime — runtime info is available per-agent.
pub async fn get_runtime_info() -> impl IntoResponse {
    info!("Runtime info requested — not available on control plane");
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({
            "error": "Runtime info is available per-agent via the Agent API. \
                      The server (control plane) does not run containers."
        })),
    )
}

/// PUT /api/v1/runtime/upgrade — runtime upgrade is handled per-agent.
pub async fn upgrade_runtime() -> impl IntoResponse {
    info!("Runtime upgrade requested — not available on control plane");
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({
            "error": "Runtime upgrade is handled per-agent via the Agent API. \
                      The server (control plane) does not manage container runtimes."
        })),
    )
}
