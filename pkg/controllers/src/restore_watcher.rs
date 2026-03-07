//! Follower restore epoch watcher.
//!
//! Polls `/registry/_restore/epoch` every 5s. When the epoch changes
//! (indicating a leader performed a cluster restore), the follower pauses
//! its controllers, reloads state from the store, and resumes.
//!
//! In a single-server deployment (current), this acts as a safety net:
//! if a restore is triggered via the API, the watcher detects the new epoch
//! and logs an informational message. In a future multi-server deployment,
//! the `on_restore_detected` callback will coordinate a full reload.

use pkg_state::client::StateStore;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tracing::{info, warn};

pub struct RestoreWatcher {
    store: StateStore,
    restore_in_progress: Arc<AtomicBool>,
}

impl RestoreWatcher {
    pub fn new(store: StateStore, restore_in_progress: Arc<AtomicBool>) -> Self {
        Self {
            store,
            restore_in_progress,
        }
    }

    /// Start the watcher loop. Returns a `JoinHandle` that can be aborted.
    pub fn start(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut last_epoch: Option<String> = self.read_epoch().await;
            let mut interval = tokio::time::interval(Duration::from_secs(
                pkg_constants::timings::RESTORE_WATCHER_INTERVAL_SECS,
            ));

            info!("Restore watcher started (initial epoch: {:?})", last_epoch);

            loop {
                interval.tick().await;

                // Skip polling while a restore is actively in progress
                if self.restore_in_progress.load(Ordering::SeqCst) {
                    continue;
                }

                let current = self.read_epoch().await;
                if current != last_epoch && current.is_some() {
                    info!(
                        "Restore epoch changed: {:?} -> {:?} — follower reload triggered",
                        last_epoch, current
                    );
                    self.on_restore_detected().await;
                    last_epoch = current;
                }
            }
        })
    }

    async fn read_epoch(&self) -> Option<String> {
        match self.store.get("/registry/_restore/epoch").await {
            Ok(Some(bytes)) => String::from_utf8(bytes).ok(),
            Ok(None) => None,
            Err(e) => {
                warn!("Restore watcher: failed to read epoch: {}", e);
                None
            }
        }
    }

    /// Called when a new restore epoch is detected.
    ///
    /// In single-server mode this is informational. In multi-server mode,
    /// this will pause controllers → reload state → resume.
    async fn on_restore_detected(&self) {
        info!("Restore detected by follower watcher — state has been refreshed by leader");
        // Future: signal controller manager to pause → reload → resume
    }
}
