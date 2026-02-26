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
    AxumPath((ns, pod_name)): AxumPath<(String, String)>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    info!("Exec request for pod {}/{}", ns, pod_name);

    // Find pod: try direct key first, then scan by name or ID
    let pod_prefix = format!("/registry/pods/{}/", ns);
    let mut found_pod: Option<pkg_types::pod::Pod> = None;

    // Direct key lookup (works when pod_name is the key/id)
    let direct_key = format!("/registry/pods/{}/{}", ns, pod_name);
    if let Ok(Some(data)) = state.store.get(&direct_key).await {
        found_pod = serde_json::from_slice::<pkg_types::pod::Pod>(&data).ok();
    }

    // If not found, scan all pods in the namespace by name or id
    if found_pod.is_none() {
        if let Ok(entries) = state.store.list_prefix(&pod_prefix).await {
            for (_, v) in entries {
                if let Ok(pod) = serde_json::from_slice::<pkg_types::pod::Pod>(&v) {
                    if pod.name == pod_name || pod.id == pod_name || pod.id.starts_with(&pod_name) {
                        found_pod = Some(pod);
                        break;
                    }
                }
            }
        }
    }

    let pod = match found_pod {
        Some(p) => p,
        None => return axum::http::StatusCode::NOT_FOUND.into_response(),
    };

    let runtime = state.container_runtime.clone();
    let container_id = pod.id.clone();
    let display_name = pod.name.clone();

    ws.on_upgrade(move |socket| {
        handle_exec_session(socket, runtime, ns, container_id, display_name)
    })
    .into_response()
}

/// Handle the WebSocket exec session.
/// Routes commands to the container runtime's exec method.
async fn handle_exec_session(
    mut socket: WebSocket,
    runtime: Arc<pkg_container::ContainerRuntime>,
    ns: String,
    container_id: String,
    display_name: String,
) {
    info!(
        "Exec session started for {}/{} (container={})",
        ns, display_name, container_id
    );

    // Send welcome message
    let backend = runtime.backend_name();
    let welcome = format!(
        "Connected to pod {}/{} (runtime: {})\r\n$ ",
        ns, display_name, backend
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

                // Execute command via runtime backend using the actual container ID
                let output = match runtime.exec_in_container(&container_id, &parts).await {
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

    info!("Exec session ended for {}/{}", ns, display_name);
}
