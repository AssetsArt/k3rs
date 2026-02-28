use axum::{
    Router,
    extract::{Path, State, ws::{Message, WebSocket, WebSocketUpgrade}},
    response::IntoResponse,
    routing::get,
};
use futures_util::{SinkExt, StreamExt};
use pkg_container::ContainerRuntime;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{error, info};

#[derive(Clone)]
pub struct AgentState {
    pub runtime: Arc<ContainerRuntime>,
}

pub fn create_agent_router(state: AgentState) -> Router {
    Router::new()
        .route("/exec/{container_id}", get(exec_handler))
        .with_state(state)
}

async fn exec_handler(
    ws: WebSocketUpgrade,
    Path(container_id): Path<String>,
    State(state): State<AgentState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, container_id, state.runtime))
}

async fn handle_socket(socket: WebSocket, container_id: String, runtime: Arc<ContainerRuntime>) {
    info!("WebSocket upgrade for container exec: {}", container_id);

    let (mut ws_sender, mut ws_receiver) = socket.split();

    // Initial message
    let _ = ws_sender
        .send(Message::Text(format!("Connecting to {}...\r\n", container_id).into()))
        .await;

    // Spawn the command in the container
    let mut child = match runtime.spawn_exec_in_container(&container_id, &[]).await {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to spawn exec: {}", e);
            let _ = ws_sender
                .send(Message::Text(format!("Error: {}\r\n", e).into()))
                .await;
            return;
        }
    };

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();

    // Channel to merge stdout and stderr
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(100);

    let tx_out = tx.clone();
    let stdout_task = tokio::spawn(async move {
        let mut buf = [0u8; 1024];
        while let Ok(n) = stdout.read(&mut buf).await {
            if n == 0 { break; }
            let text = String::from_utf8_lossy(&buf[..n]).to_string();
            if tx_out.send(text).await.is_err() { break; }
        }
    });

    let tx_err = tx.clone();
    let stderr_task = tokio::spawn(async move {
        let mut buf = [0u8; 1024];
        while let Ok(n) = stderr.read(&mut buf).await {
            if n == 0 { break; }
            let text = String::from_utf8_lossy(&buf[..n]).to_string();
            if tx_err.send(text).await.is_err() { break; }
        }
    });

    // Task to send from channel to WebSocket
    let ws_send_task = tokio::spawn(async move {
        while let Some(text) = rx.recv().await {
            if ws_sender.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
    });

    // Pipe WebSocket -> stdin
    let stdin_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_receiver.next().await {
            match msg {
                Message::Text(text) => {
                    if text == "exit" { break; }
                    if stdin.write_all(text.as_bytes()).await.is_err() {
                        break;
                    }
                    if stdin.write_all(b"\n").await.is_err() {
                        break;
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    // Wait for tasks or child process
    tokio::select! {
        _ = stdout_task => {},
        _ = stderr_task => {},
        _ = ws_send_task => {},
        _ = stdin_task => {},
        _ = child.wait() => {
            info!("Exec process finished for {}", container_id);
        },
    }
}
