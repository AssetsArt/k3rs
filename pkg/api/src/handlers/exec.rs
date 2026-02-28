use axum::{
    extract::{
        Path as AxumPath, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use pkg_types::node::Node;
use pkg_types::pod::Pod;
use tracing::{error, info, warn};

use crate::AppState;

/// WebSocket-based exec endpoint for attaching to containers.
///
/// On the Server (Control Plane), this acts as a proxy to the Agent
/// node where the pod is actually running.
pub async fn exec_into_pod(
    AxumPath((ns, pod_name)): AxumPath<(String, String)>,
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    info!("Exec request for pod {}/{}", ns, pod_name);

    // 1. Find the pod in the state store
    let pod_key = format!("/registry/pods/{}/{}", ns, pod_name);
    info!("Looking up pod with key: {}", pod_key);
    let pod: Pod = match state.store.get(&pod_key).await {
        Ok(Some(data)) => match serde_json::from_slice(&data) {
            Ok(p) => p,
            Err(e) => {
                error!("Failed to deserialize pod: {}", e);
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        },
        Ok(None) => {
            warn!("Pod not found in store: {}", pod_key);
            return StatusCode::NOT_FOUND.into_response();
        }
        Err(e) => {
            error!("Store error during pod lookup: {}", e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    // 2. Find the node where the pod is running
    let node_name = match &pod.node_name {
        Some(name) => name,
        None => return (StatusCode::BAD_REQUEST, "Pod is not scheduled to a node").into_response(),
    };

    // pod.node_name stores the node UUID (set by the scheduler), but nodes are
    // stored under /registry/nodes/{node_name} (human name). Scan all nodes to
    // find the one whose id matches.
    let node_entries = match state.store.list_prefix("/registry/nodes/").await {
        Ok(e) => e,
        Err(e) => {
            error!("Store error during node scan: {}", e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let node: Node = match node_entries
        .into_iter()
        .filter_map(|(_, v)| serde_json::from_slice::<Node>(&v).ok())
        .find(|n| n.id == *node_name)
    {
        Some(n) => n,
        None => {
            warn!("Node with id {} not found in registry", node_name);
            return (StatusCode::NOT_FOUND, "Node not found").into_response();
        }
    };

    let agent_url = format!(
        "ws://{}:{}/exec/{}",
        node.address, node.agent_api_port, pod.id
    );

    // 3. Upgrade and proxy
    ws.on_upgrade(move |socket| proxy_to_agent(socket, agent_url))
}

async fn proxy_to_agent(mut client_socket: WebSocket, agent_url: String) {
    info!("Proxying exec session to agent: {}", agent_url);

    // Connect to the agent node's WebSocket API
    let (agent_socket, _) = match tokio_tungstenite::connect_async(&agent_url).await {
        Ok(conn) => conn,
        Err(e) => {
            error!("Failed to connect to agent WebSocket: {}", e);
            let _ = client_socket
                .send(Message::Text(
                    format!("Error: Failed to connect to agent: {}\r\n", e).into(),
                ))
                .await;
            return;
        }
    };

    let (mut client_sender, mut client_receiver) = client_socket.split();
    let (mut agent_sender, mut agent_receiver) = agent_socket.split();

    // Client -> Agent
    let client_to_agent = tokio::spawn(async move {
        while let Some(Ok(msg)) = client_receiver.next().await {
            // Map axum message to tungstenite message
            let t_msg = match msg {
                Message::Text(t) => {
                    tokio_tungstenite::tungstenite::Message::Text(t.as_str().into())
                }
                Message::Binary(b) => tokio_tungstenite::tungstenite::Message::Binary(b.into()),
                Message::Ping(p) => tokio_tungstenite::tungstenite::Message::Ping(p.into()),
                Message::Pong(p) => tokio_tungstenite::tungstenite::Message::Pong(p.into()),
                Message::Close(_) => break,
            };
            if agent_sender.send(t_msg).await.is_err() {
                break;
            }
        }
    });

    // Agent -> Client
    let agent_to_client = tokio::spawn(async move {
        while let Some(Ok(msg)) = agent_receiver.next().await {
            // Map tungstenite message to axum message
            let a_msg = match msg {
                tokio_tungstenite::tungstenite::Message::Text(t) => {
                    Message::Text(t.as_str().into())
                }
                tokio_tungstenite::tungstenite::Message::Binary(b) => Message::Binary(b.into()),
                tokio_tungstenite::tungstenite::Message::Ping(p) => Message::Ping(p.into()),
                tokio_tungstenite::tungstenite::Message::Pong(p) => Message::Pong(p.into()),
                _ => break,
            };
            if client_sender.send(a_msg).await.is_err() {
                break;
            }
        }
    });

    // Wait for either to finish
    tokio::select! {
        _ = client_to_agent => {
            warn!("Exec proxy: client connection closed");
        },
        _ = agent_to_client => {
            warn!("Exec proxy: agent connection closed");
        },
    }
}
