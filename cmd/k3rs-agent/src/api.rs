//! Agent WebSocket exec handler.
//!
//! For interactive sessions (`tty=true`) we create a real PTY pair via
//! `nix::pty::openpty`, spawn the OCI runtime with the slave as the process
//! stdin/stdout/stderr, and bridge the PTY master to/from the WebSocket.
//! This gives the shell a proper terminal: prompts, echo, Ctrl+C, colours, etc.
//!
//! For non-interactive commands (`tty=false`) we use plain pipes and wait for
//! the child to exit before closing the connection (avoid the race where
//! child.wait() fires before all buffered output is sent).

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

    // Initial "connecting" text frame — client drains this before entering raw mode.
    let _ = ws_sender
        .send(Message::Text(
            format!("Connecting to {}...\r\n", container_id).into(),
        ))
        .await;

    let cmd_refs: Vec<&str> = command.iter().map(String::as_str).collect();

    if tty {
        handle_tty(ws_sender, ws_receiver, container_id, cmd_refs, runtime).await;
    } else {
        handle_pipe(ws_sender, ws_receiver, container_id, cmd_refs, runtime).await;
    }
}

// ─── PTY / interactive mode ──────────────────────────────────────────────────

async fn handle_tty(
    mut ws_sender: futures_util::stream::SplitSink<WebSocket, Message>,
    mut ws_receiver: futures_util::stream::SplitStream<WebSocket>,
    container_id: String,
    command: Vec<&str>,
    runtime: Arc<ContainerRuntime>,
) {
    // We need to know how to spawn a command inside the container.
    // Prefer nsenter (no cgroup permissions needed) over youki exec.
    // Get the container's main process PID (written by youki at create time).
    let container_pid = runtime.container_pid(&container_id);
    let runtime_bin = runtime.oci_runtime_path();

    if container_pid.is_none() && runtime_bin.is_none() {
        let _ = ws_sender
            .send(Message::Text(
                "Error: PTY exec not supported on this backend\r\n".into(),
            ))
            .await;
        return;
    }

    // Create a PTY pair using libc::openpty.
    // master = our side (we read/write raw terminal bytes)
    // slave  = container process side (its stdin/stdout/stderr)
    let (master_fd, slave_fd) = {
        let mut master: libc::c_int = -1;
        let mut slave: libc::c_int = -1;
        let ret = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        if ret != 0 {
            let err = std::io::Error::last_os_error();
            error!("openpty failed: {}", err);
            let _ = ws_sender
                .send(Message::Text(
                    format!("Error: openpty failed: {}\r\n", err).into(),
                ))
                .await;
            return;
        }
        (master, slave)
    };

    // Build the exec command: nsenter (preferred, avoids cgroup permission issues)
    // or youki exec as fallback.
    let mut cmd_args_owned: Vec<String>;
    let (bin, bin_args): (&str, Vec<String>);

    if let Some(pid) = container_pid {
        // nsenter: enters the container's namespaces by PID, no cgroup interaction needed.
        cmd_args_owned = vec![
            "--target".to_string(),
            pid.to_string(),
            "--pid".to_string(),
            "--uts".to_string(),
            "--ipc".to_string(),
            "--net".to_string(),
            "--mount".to_string(),
            "--user".to_string(),
            "--preserve-credentials".to_string(),
            "--".to_string(),
        ];
        if command.is_empty() {
            cmd_args_owned.push("/bin/sh".to_string());
        } else {
            cmd_args_owned.extend(command.iter().map(|s| s.to_string()));
        }
        bin = "nsenter";
        bin_args = cmd_args_owned.clone();
    } else {
        let rb = runtime_bin.as_deref().unwrap_or("");
        cmd_args_owned = vec!["exec".to_string(), container_id.clone()];
        if command.is_empty() {
            cmd_args_owned.push("/bin/sh".to_string());
        } else {
            cmd_args_owned.extend(command.iter().map(|s| s.to_string()));
        }
        bin = rb;
        bin_args = cmd_args_owned.clone();
    }

    // Each Stdio must own a distinct fd — passing the same raw fd to from_raw_fd
    // three times creates three owners, causing a triple-close (IO safety abort).
    // Dup slave_fd for stdout and stderr; stdin takes ownership of the original.
    // All three are closed automatically when Command::spawn() drops the Stdio
    // objects, so the parent holds no slave-side reference after spawn() returns
    // (which is what lets reads on the master return EOF when the child exits).
    let child = unsafe {
        use std::os::unix::io::FromRawFd;
        use std::os::unix::process::CommandExt;
        use std::process::Stdio;
        tokio::process::Command::new(bin)
            .args(&bin_args)
            .env("TERM", "xterm-256color")
            .stdin(Stdio::from_raw_fd(slave_fd))
            .stdout(Stdio::from_raw_fd(libc::dup(slave_fd)))
            .stderr(Stdio::from_raw_fd(libc::dup(slave_fd)))
            .pre_exec(|| {
                // Create a new session so we can acquire a controlling terminal.
                libc::setsid();
                // Make the slave PTY the controlling terminal of this session.
                // Without this, shells print "can't access tty" and disable job control.
                libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 0);
                Ok(())
            })
            .spawn()
    };

    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to spawn exec: {}", e);
            let _ = ws_sender
                .send(Message::Text(format!("Error: {}\r\n", e).into()))
                .await;
            unsafe {
                libc::close(master_fd);
            }
            return;
        }
    };

    // tokio::fs::File has a single internal state machine: while a blocking read
    // is in-flight (Busy state), poll_write waits for the read to finish before
    // it can issue a write. On a PTY this is a deadlock — the read waits for
    // output that only appears after a write carries the keystroke in.
    // Fix: dup the master fd so reads and writes use independent File objects
    // with independent state machines that never block each other.
    let mut master_read = unsafe {
        use std::os::unix::io::FromRawFd;
        tokio::fs::File::from_raw_fd(master_fd)
    };
    let mut master_write = unsafe {
        use std::os::unix::io::FromRawFd;
        tokio::fs::File::from_raw_fd(libc::dup(master_fd))
    };

    let (output_tx, mut output_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);

    // Task: PTY master → channel → WebSocket
    let read_task = tokio::spawn(async move {
        let mut buf = [0u8; 1024];
        loop {
            match master_read.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if output_tx.send(buf[..n].to_vec()).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    let ws_send_task = tokio::spawn(async move {
        while let Some(bytes) = output_rx.recv().await {
            if ws_sender.send(Message::Binary(bytes.into())).await.is_err() {
                break;
            }
        }
    });

    // Task: WebSocket → PTY master
    let write_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_receiver.next().await {
            match msg {
                Message::Binary(bytes) => {
                    if master_write.write_all(&bytes).await.is_err() {
                        break;
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    tokio::select! {
        _ = read_task => {},
        _ = ws_send_task => {},
        _ = write_task => {},
        _ = child.wait() => {
            info!("PTY exec process exited for {}", container_id);
        },
    }
}

// ─── Pipe / non-interactive mode ─────────────────────────────────────────────

async fn handle_pipe(
    mut ws_sender: futures_util::stream::SplitSink<WebSocket, Message>,
    mut ws_receiver: futures_util::stream::SplitStream<WebSocket>,
    container_id: String,
    command: Vec<&str>,
    runtime: Arc<ContainerRuntime>,
) {
    let mut child = match runtime
        .spawn_exec_in_container(&container_id, &command, false)
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

    // Drop original tx so the channel closes when BOTH reader tasks finish.
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

    // Drop the original sender — channel closes only when all clones are gone.
    drop(tx);

    // WS stdin task.
    let stdin_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_receiver.next().await {
            match msg {
                Message::Text(text) => {
                    if text == "exit" {
                        break;
                    }
                    let _ = stdin.write_all(text.as_bytes()).await;
                    let _ = stdin.write_all(b"\n").await;
                }
                Message::Binary(bytes) => {
                    let _ = stdin.write_all(&bytes).await;
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    // Wait for child to exit FIRST, then drain all output.
    let _ = child.wait().await;
    info!("Exec process exited for {}", container_id);

    // stdout/stderr pipes will reach EOF now that child is gone.
    let _ = tokio::join!(stdout_task, stderr_task);
    stdin_task.abort();

    // Drain remaining buffered output and send a Close frame.
    while let Some(bytes) = rx.recv().await {
        let text = String::from_utf8_lossy(&bytes).into_owned();
        if ws_sender.send(Message::Text(text.into())).await.is_err() {
            break;
        }
    }

    let _ = ws_sender.send(Message::Close(None)).await;
}
