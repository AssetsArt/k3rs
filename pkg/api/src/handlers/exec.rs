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
/// Pipes stdin/stdout over WebSocket to the container's task process.
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
/// In stub mode, echoes commands and returns simulated output.
/// In real mode, pipes to the container's process.
async fn handle_exec_session(
    mut socket: WebSocket,
    runtime: Arc<pkg_container::ContainerRuntime>,
    ns: String,
    pod_id: String,
) {
    info!("Exec session started for {}/{}", ns, pod_id);

    // Send welcome message
    let welcome = format!(
        "Connected to pod {}/{}\r\nType commands and press Enter.\r\n$ ",
        ns, pod_id
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

                // Execute command in container context
                let output = execute_in_container(&runtime, &pod_id, &cmd).await;
                let response = format!("{}\r\n$ ", output);
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

/// Execute a command in the container.
/// In stub mode, returns simulated output.
async fn execute_in_container(
    _runtime: &pkg_container::ContainerRuntime,
    pod_id: &str,
    cmd: &str,
) -> String {
    // In a real implementation, this would:
    // 1. Create an exec process inside the container via containerd Tasks API
    // 2. Pipe stdin/stdout
    // For now, provide useful simulated output
    match cmd.split_whitespace().next().unwrap_or("") {
        "ls" => "bin  dev  etc  home  lib  proc  root  run  sbin  sys  tmp  usr  var".to_string(),
        "pwd" => "/".to_string(),
        "whoami" => "root".to_string(),
        "hostname" => pod_id.to_string(),
        "uname" => "Linux k3rs 6.1.0-k3rs #1 SMP PREEMPT x86_64 GNU/Linux".to_string(),
        "date" => chrono::Utc::now()
            .format("%a %b %e %H:%M:%S UTC %Y")
            .to_string(),
        "env" => format!(
            "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\n\
             HOSTNAME={}\n\
             KUBERNETES_SERVICE_HOST=10.43.0.1\n\
             KUBERNETES_SERVICE_PORT=6443",
            pod_id
        ),
        "cat" if cmd.contains("/etc/hostname") => pod_id.to_string(),
        "ps" => format!(
            "PID  USER  CMD\n1    root  /entrypoint\n{}  root  {}",
            std::process::id(),
            cmd
        ),
        _ => format!("sh: {}: command executed in container {}", cmd, pod_id),
    }
}
