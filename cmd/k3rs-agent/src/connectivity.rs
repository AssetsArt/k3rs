use std::fmt;
use tokio::sync::watch;
use tracing::{info, warn};

/// Agent-to-server connectivity state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectivityState {
    /// Initial startup — attempting first connection.
    Connecting,
    /// Server reachable. All syncs succeeding.
    Connected,
    /// Was connected, now failing. Serving stale in-memory state.
    Reconnecting { attempt: u32 },
    /// Server unreachable at startup. Serving from cache if available.
    Offline,
}

impl fmt::Display for ConnectivityState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connecting => write!(f, "CONNECTING"),
            Self::Connected => write!(f, "CONNECTED"),
            Self::Reconnecting { attempt } => write!(f, "RECONNECTING (attempt {})", attempt),
            Self::Offline => write!(f, "OFFLINE"),
        }
    }
}

/// Manages connectivity state and broadcasts changes via `watch` channel.
pub struct ConnectivityManager {
    tx: watch::Sender<ConnectivityState>,
    rx: watch::Receiver<ConnectivityState>,
}

impl ConnectivityManager {
    pub fn new() -> Self {
        let (tx, rx) = watch::channel(ConnectivityState::Connecting);
        Self { tx, rx }
    }

    #[allow(dead_code)]
    pub fn state(&self) -> ConnectivityState {
        *self.rx.borrow()
    }

    pub fn set_connected(&self) {
        let prev = *self.rx.borrow();
        if prev != ConnectivityState::Connected {
            info!("Connectivity: {} -> CONNECTED", prev);
            let _ = self.tx.send(ConnectivityState::Connected);
        }
    }

    pub fn set_reconnecting(&self, attempt: u32) {
        let new_state = ConnectivityState::Reconnecting { attempt };
        warn!("Connectivity: {} -> {}", *self.rx.borrow(), new_state);
        let _ = self.tx.send(new_state);
    }

    pub fn set_offline(&self) {
        let prev = *self.rx.borrow();
        if prev != ConnectivityState::Offline {
            warn!("Connectivity: {} -> OFFLINE", prev);
            let _ = self.tx.send(ConnectivityState::Offline);
        }
    }

    /// Returns true when the server is reachable.
    pub fn is_connected(&self) -> bool {
        matches!(*self.rx.borrow(), ConnectivityState::Connected)
    }

    /// Exponential backoff: 1s → 2s → 4s → … → 30s (capped).
    pub fn backoff_duration(attempt: u32) -> std::time::Duration {
        let secs = std::cmp::min(1u64 << attempt, 30);
        std::time::Duration::from_secs(secs)
    }
}
