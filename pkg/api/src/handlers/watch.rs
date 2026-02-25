use axum::{
    extract::{Query, State},
    response::sse::{Event, KeepAlive, Sse},
};
use serde::Deserialize;
use std::convert::Infallible;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;
use tracing::info;

use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct WatchQuery {
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub seq: Option<u64>,
}

/// GET /api/v1/watch — SSE endpoint streaming watch events.
pub async fn watch_events(
    State(state): State<AppState>,
    Query(query): Query<WatchQuery>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let prefix = query.prefix.unwrap_or_default();
    let from_seq = query.seq.unwrap_or(0);

    info!(
        "Watch subscription: prefix='{}', from_seq={}",
        prefix, from_seq
    );

    // First send any buffered events since from_seq
    let buffered = state.store.event_log.events_since(from_seq).await;

    let rx = state.store.event_log.subscribe();
    let stream = BroadcastStream::new(rx);

    let prefix_clone = prefix.clone();

    // Create a combined stream: buffered events first, then live events
    let buffered_stream = tokio_stream::iter(
        buffered
            .into_iter()
            .filter(move |e| prefix.is_empty() || e.key.starts_with(&prefix))
            .map(|e| {
                let data = serde_json::to_string(&e).unwrap_or_default();
                Ok::<_, Infallible>(Event::default().data(data))
            }),
    );

    let live_stream = stream.filter_map(move |result| match result {
        Ok(event) => {
            if prefix_clone.is_empty() || event.key.starts_with(&prefix_clone) {
                if let Ok(data) = serde_json::to_string(&event) {
                    return Some(Ok::<_, Infallible>(Event::default().data(data)));
                }
            }
            None
        }
        Err(_) => None,
    });

    let combined = buffered_stream.chain(live_stream);

    Sse::new(combined).keep_alive(KeepAlive::default())
}
