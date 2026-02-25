use axum::{
    extract::{
        Path as AxumPath, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    response::IntoResponse,
};
use std::sync::Arc;
use tracing::info;

use crate::AppState;

/// WebSocket-based exec endpoint for attaching to containers.
/// Pipes stdin/stdout over WebSocket to the container runtime.
pub async fn exec_into_pod(
    State(state): State<AppState>,
    AxumPath((ns, pod_id)): AxumPath<(String, String)>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    info!("Exec request for pod {}/{}", ns, pod_id);

    // Verify pod exists
    let key = format!("/registry/pods/{}/{}", ns, pod_id);
    let pod_exists = state.store.get(&key).await.ok().flatten().is_some();

    if !pod_exists {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    }

    let runtime = state.container_runtime.clone();
    let pod_id_clone = pod_id.clone();

    ws.on_upgrade(move |socket| handle_exec_session(socket, runtime, ns, pod_id_clone))
        .into_response()
}

/// Handle the WebSocket exec session.
/// Routes commands to the container runtime's exec method.
async fn handle_exec_session(
    mut socket: WebSocket,
    runtime: Arc<pkg_container::ContainerRuntime>,
    ns: String,
    pod_id: String,
) {
    info!("Exec session started for {}/{}", ns, pod_id);

    // Send welcome message
    let backend = runtime.backend_name();
    let welcome = format!(
        "Connected to pod {}/{} (runtime: {})\r\n$ ",
        ns, pod_id, backend
    );
    if socket.send(Message::Text(welcome.into())).await.is_err() {
        return;
    }

    // Process incoming messages
    while let Some(Ok(msg)) = socket.recv().await {
        match msg {
            Message::Text(text) => {
                let cmd = text.trim().to_string();
                if cmd.is_empty() {
                    let _ = socket.send(Message::Text("$ ".into())).await;
                    continue;
                }

                if cmd == "exit" || cmd == "quit" {
                    let _ = socket
                        .send(Message::Text("Disconnecting...\r\n".into()))
                        .await;
                    break;
                }

                // Parse command into parts
                let parts: Vec<&str> = cmd.split_whitespace().collect();
                if parts.is_empty() {
                    let _ = socket.send(Message::Text("$ ".into())).await;
                    continue;
                }

                // Execute command via runtime backend
                let output = match runtime.exec_in_container(&pod_id, &parts).await {
                    Ok(out) => out,
                    Err(e) => format!("exec error: {}\r\n", e),
                };

                let response = format!("{}\r\n$ ", output.trim_end());
                if socket.send(Message::Text(response.into())).await.is_err() {
                    break;
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    info!("Exec session ended for {}/{}", ns, pod_id);
}
