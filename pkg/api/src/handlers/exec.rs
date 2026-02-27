use axum::{
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::IntoResponse,
};
use tracing::info;

use crate::AppState;

/// WebSocket-based exec endpoint for attaching to containers.
///
/// On the Server (Control Plane), exec is not available directly.
/// The exec request must be proxied to the Agent node where the pod is running.
pub async fn exec_into_pod(
    State(_state): State<AppState>,
    AxumPath((ns, pod_name)): AxumPath<(String, String)>,
) -> impl IntoResponse {
    info!(
        "Exec request for pod {}/{} — not available on control plane",
        ns, pod_name
    );
    (
        StatusCode::NOT_IMPLEMENTED,
        "Exec is handled by the Agent node where the pod is running. \
         Use the Agent API to exec into containers.",
    )
}
