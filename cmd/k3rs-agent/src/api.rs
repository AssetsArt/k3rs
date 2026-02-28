use axum::{
    Router,
    extract::{
        Path, Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::IntoResponse,
    routing::get,
};
use futures_util::{SinkExt, StreamExt};
use pkg_container::ContainerRuntime;
use serde::Deserialize;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{error, info};

#[derive(Clone)]
pub struct AgentState {
    pub runtime: Arc<ContainerRuntime>,
}

#[derive(Debug, Deserialize)]
pub struct ExecQuery {
    /// Space-separated command to run inside the container.
    /// If absent or empty, defaults to /bin/sh.
    #[serde(default)]
    pub cmd: String,
    /// Allocate a pseudo-terminal (PTY) inside the container.
    /// When true, raw bytes are exchanged via Binary WebSocket messages.
    #[serde(default)]
    pub tty: bool,
}

pub fn create_agent_router(state: AgentState) -> Router {
    Router::new()
        .route("/exec/{container_id}", get(exec_handler))
        .with_state(state)
}

async fn exec_handler(
    ws: WebSocketUpgrade,
    Path(container_id): Path<String>,
    Query(query): Query<ExecQuery>,
    State(state): State<AgentState>,
) -> impl IntoResponse {
    let command: Vec<String> = if query.cmd.is_empty() {
        vec![]
    } else {
        query.cmd.split_whitespace().map(String::from).collect()
    };
    let tty = query.tty;
    ws.on_upgrade(move |socket| handle_socket(socket, container_id, command, tty, state.runtime))
}

async fn handle_socket(
    socket: WebSocket,
    container_id: String,
    command: Vec<String>,
    tty: bool,
    runtime: Arc<ContainerRuntime>,
) {
    info!("WebSocket exec: container={} tty={}", container_id, tty);

    let (mut ws_sender, mut ws_receiver) = socket.split();

    // Send a welcome text frame so the client knows we're connected.
    let _ = ws_sender
        .send(Message::Text(
            format!("Connecting to {}...\r\n", container_id).into(),
        ))
        .await;

    // Spawn the process in the container.
    let cmd_refs: Vec<&str> = command.iter().map(String::as_str).collect();
    let mut child = match runtime
        .spawn_exec_in_container(&container_id, &cmd_refs, tty)
        .await
    {
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

    // Channel to merge stdout + stderr → WebSocket sender.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);

    let tx_out = tx.clone();
    let stdout_task = tokio::spawn(async move {
        let mut buf = [0u8; 1024];
        loop {
            match stdout.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx_out.send(buf[..n].to_vec()).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    let tx_err = tx.clone();
    let stderr_task = tokio::spawn(async move {
        let mut buf = [0u8; 1024];
        loop {
            match stderr.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx_err.send(buf[..n].to_vec()).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    // Forward process output → WebSocket (Binary in tty mode, Text otherwise).
    let ws_send_task = tokio::spawn(async move {
        while let Some(bytes) = rx.recv().await {
            let msg = if tty {
                Message::Binary(bytes.into())
            } else {
                Message::Text(String::from_utf8_lossy(&bytes).into_owned().into())
            };
            if ws_sender.send(msg).await.is_err() {
                break;
            }
        }
    });

    // Forward WebSocket → process stdin.
    let stdin_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_receiver.next().await {
            match msg {
                // tty mode: raw bytes from the client's terminal
                Message::Binary(bytes) => {
                    if stdin.write_all(&bytes).await.is_err() {
                        break;
                    }
                }
                // text mode: line-by-line commands
                Message::Text(text) => {
                    if text == "exit" {
                        break;
                    }
                    if stdin.write_all(text.as_bytes()).await.is_err() {
                        break;
                    }
                    // Append newline only in non-tty text mode.
                    if !tty && stdin.write_all(b"\n").await.is_err() {
                        break;
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    tokio::select! {
        _ = stdout_task => {},
        _ = stderr_task => {},
        _ = ws_send_task => {},
        _ = stdin_task => {},
        _ = child.wait() => {
            info!("Exec process exited for {}", container_id);
        },
    }
}
