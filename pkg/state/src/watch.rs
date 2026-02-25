use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::sync::broadcast;

/// Type of event in the watch stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EventType {
    Put,
    Delete,
}

/// A single watch event representing a state change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchEvent {
    pub seq: u64,
    pub event_type: EventType,
    pub key: String,
    #[serde(default)]
    pub value: Option<Vec<u8>>,
}

/// In-memory event log that tracks all state mutations with monotonic sequence numbers.
/// Clients can subscribe to receive events filtered by key prefix.
#[derive(Clone)]
pub struct EventLog {
    inner: Arc<RwLock<EventLogInner>>,
    sender: broadcast::Sender<WatchEvent>,
}

struct EventLogInner {
    seq: u64,
    /// Ring buffer of recent events (capped)
    events: Vec<WatchEvent>,
    max_events: usize,
}

impl EventLog {
    /// Create a new event log with the given capacity for recent events.
    pub fn new(max_events: usize) -> Self {
        let (sender, _) = broadcast::channel(1024);
        Self {
            inner: Arc::new(RwLock::new(EventLogInner {
                seq: 0,
                events: Vec::with_capacity(max_events),
                max_events,
            })),
            sender,
        }
    }

    /// Record a new event. Called internally by StateStore on put/delete.
    pub async fn emit(&self, event_type: EventType, key: String, value: Option<Vec<u8>>) {
        let mut inner = self.inner.write().await;
        inner.seq += 1;
        let event = WatchEvent {
            seq: inner.seq,
            event_type,
            key,
            value,
        };
        // Ring buffer: remove oldest if at capacity
        if inner.events.len() >= inner.max_events {
            inner.events.remove(0);
        }
        inner.events.push(event.clone());
        // Broadcast to subscribers (ignore errors if no receivers)
        let _ = self.sender.send(event);
    }

    /// Get the current sequence number.
    pub async fn current_seq(&self) -> u64 {
        self.inner.read().await.seq
    }

    /// Get all events since the given sequence number.
    pub async fn events_since(&self, from_seq: u64) -> Vec<WatchEvent> {
        let inner = self.inner.read().await;
        inner
            .events
            .iter()
            .filter(|e| e.seq > from_seq)
            .cloned()
            .collect()
    }

    /// Subscribe to receive new events as they are emitted.
    pub fn subscribe(&self) -> broadcast::Receiver<WatchEvent> {
        self.sender.subscribe()
    }
}
