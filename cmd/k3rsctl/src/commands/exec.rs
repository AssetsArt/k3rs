use futures_util::{SinkExt, StreamExt};

pub async fn handle(
    server: &str,
    token: &str,
    pod_id: &str,
    command: &[String],
    namespace: &str,
    interactive: bool,
) -> anyhow::Result<()> {
    // --it / -i flag drives interactive mode.
    let is_interactive = interactive || command.is_empty();

    // Build URL — encode and pass the command as a ?cmd= query param so
    // the agent spawns it directly rather than piping it as stdin.
    let ws_url = server
        .replace("http://", "ws://")
        .replace("https://", "wss://");

    let cmd_str = command.join(" ");
    let encoded_cmd: String = cmd_str
        .chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' | '/' => c.to_string(),
            ' ' => "%20".to_string(),
            c => format!("%{:02X}", c as u32),
        })
        .collect();

    let mut params = Vec::new();
    if !encoded_cmd.is_empty() {
        params.push(format!("cmd={}", encoded_cmd));
    }
    if is_interactive {
        params.push("tty=true".to_string());
    }
    let url = if params.is_empty() {
        format!(
            "{}/api/v1/namespaces/{}/pods/{}/exec",
            ws_url, namespace, pod_id
        )
    } else {
        format!(
            "{}/api/v1/namespaces/{}/pods/{}/exec?{}",
            ws_url,
            namespace,
            pod_id,
            params.join("&")
        )
    };

    let request = tokio_tungstenite::tungstenite::http::Request::builder()
        .uri(&url)
        .header("Authorization", format!("Bearer {}", token))
        .header("Host", "localhost")
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header(
            "Sec-WebSocket-Key",
            tokio_tungstenite::tungstenite::handshake::client::generate_key(),
        )
        .body(())
        .expect("Failed to build WebSocket request");

    let (ws_stream, _) = match tokio_tungstenite::connect_async(request).await {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("Failed to connect WebSocket: {}", e);
            std::process::exit(1);
        }
    };

    let (mut write, mut read) = ws_stream.split();

    if is_interactive {
        handle_interactive(&mut write, &mut read).await;
    } else {
        handle_non_interactive(&mut write, &mut read).await;
    }

    Ok(())
}

async fn handle_interactive<W, R>(write: &mut W, read: &mut R)
where
    W: futures_util::Sink<tokio_tungstenite::tungstenite::Message> + Unpin,
    R: futures_util::Stream<
            Item = Result<
                tokio_tungstenite::tungstenite::Message,
                tokio_tungstenite::tungstenite::Error,
            >,
        > + Unpin,
{
    // Enable raw mode: keystrokes sent immediately, no local echo.
    crossterm::terminal::enable_raw_mode().expect("Failed to enable raw terminal mode");

    // Drain the "Connecting to …" welcome text frame → print to stderr
    if let Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) = read.next().await {
        eprint!("{}", text);
    }

    // Spawn a blocking thread to read raw stdin bytes.
    let (stdin_tx, mut stdin_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(32);
    std::thread::spawn(move || {
        use std::io::Read as _;
        let mut buf = [0u8; 256];
        loop {
            match std::io::stdin().read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if stdin_tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    // Bidirectional raw tunnel.
    // Double Ctrl+C (0x03) within 1 second exits the session.
    let mut last_ctrl_c: Option<std::time::Instant> = None;
    loop {
        tokio::select! {
            // Keystrokes from local terminal → container
            bytes = stdin_rx.recv() => {
                match bytes {
                    Some(b) => {
                        if b.contains(&0x03) {
                            if last_ctrl_c
                                .map(|t| t.elapsed() < std::time::Duration::from_secs(1))
                                .unwrap_or(false)
                            {
                                break;
                            }
                            last_ctrl_c = Some(std::time::Instant::now());
                        } else {
                            last_ctrl_c = None;
                        }
                        if write
                            .send(tokio_tungstenite::tungstenite::Message::Binary(
                                b.into(),
                            ))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    None => break,
                }
            }
            // Output from container → local terminal
            msg = read.next() => {
                match msg {
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Binary(b))) => {
                        use std::io::Write as _;
                        std::io::stdout().write_all(&b).ok();
                        std::io::stdout().flush().ok();
                    }
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Text(t))) => {
                        eprint!("{}", t);
                    }
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_)))
                    | None => break,
                    _ => {}
                }
            }
        }
    }

    // Restore the terminal before exiting.
    crossterm::terminal::disable_raw_mode().ok();
    eprintln!("\r\nSession closed.");
}

async fn handle_non_interactive<W, R>(write: &mut W, read: &mut R)
where
    W: futures_util::Sink<tokio_tungstenite::tungstenite::Message> + Unpin,
    R: futures_util::Stream<
            Item = Result<
                tokio_tungstenite::tungstenite::Message,
                tokio_tungstenite::tungstenite::Error,
            >,
        > + Unpin,
{
    // Drain the "Connecting to …" welcome message.
    if let Some(Ok(tokio_tungstenite::tungstenite::Message::Text(_))) = read.next().await {}

    loop {
        match read.next().await {
            Some(Ok(tokio_tungstenite::tungstenite::Message::Binary(b))) => {
                use std::io::Write as _;
                std::io::stdout().write_all(&b).ok();
                std::io::stdout().flush().ok();
            }
            Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                print!("{}", text);
            }
            Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) | None => break,
            _ => {}
        }
    }

    let _ = write
        .send(tokio_tungstenite::tungstenite::Message::Close(None))
        .await;
}
