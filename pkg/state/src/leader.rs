use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::sync::watch;
use tracing::{info, warn};

use crate::client::StateStore;

use pkg_constants::state::{
    LEADER_LEASE_KEY, LEADER_LEASE_TTL_SECS, LEADER_RENEW_INTERVAL_DIVISOR,
};

/// A distributed lease for leader election.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lease {
    pub id: String,
    pub holder_id: String,
    pub acquired_at: chrono::DateTime<Utc>,
    pub renew_at: chrono::DateTime<Utc>,
    pub ttl_seconds: u64,
}

impl Lease {
    pub fn is_expired(&self) -> bool {
        let expiry = self.renew_at + chrono::Duration::seconds(self.ttl_seconds as i64);
        Utc::now() > expiry
    }
}

/// Leader election engine using SlateDB leases.
///
/// Only one server instance holds the lease at a time.
/// The leader runs Scheduler and Controllers; followers only serve API reads.
pub struct LeaderElection {
    store: StateStore,
    server_id: String,
    ttl: Duration,
    renew_interval: Duration,
    leader_tx: watch::Sender<bool>,
    leader_rx: watch::Receiver<bool>,
}

impl LeaderElection {
    pub fn new(store: StateStore, server_id: String) -> Self {
        let ttl = Duration::from_secs(LEADER_LEASE_TTL_SECS);
        let renew_interval =
            Duration::from_secs(LEADER_LEASE_TTL_SECS / LEADER_RENEW_INTERVAL_DIVISOR);
        let (leader_tx, leader_rx) = watch::channel(false);

        Self {
            store,
            server_id,
            ttl,
            renew_interval,
            leader_tx,
            leader_rx,
        }
    }

    /// Get a receiver to observe leadership changes.
    pub fn subscribe(&self) -> watch::Receiver<bool> {
        self.leader_rx.clone()
    }

    /// Check if this instance is currently the leader.
    pub fn is_leader(&self) -> bool {
        *self.leader_rx.borrow()
    }

    /// Try to acquire or renew the lease. Returns true if we are the leader.
    async fn try_acquire_or_renew(&self) -> anyhow::Result<bool> {
        let now = Utc::now();

        match self.store.get(LEADER_LEASE_KEY).await? {
            Some(data) => {
                let lease: Lease = serde_json::from_slice(&data)?;

                if lease.holder_id == self.server_id {
                    // We hold it — renew
                    let renewed = Lease {
                        renew_at: now,
                        ..lease
                    };
                    let data = serde_json::to_vec(&renewed)?;
                    self.store.put(LEADER_LEASE_KEY, &data).await?;
                    Ok(true)
                } else if lease.is_expired() {
                    // Previous holder's lease expired — take over
                    info!(
                        "Lease expired (held by {}), acquiring for {}",
                        lease.holder_id, self.server_id
                    );
                    let new_lease = Lease {
                        id: "controller-leader".to_string(),
                        holder_id: self.server_id.clone(),
                        acquired_at: now,
                        renew_at: now,
                        ttl_seconds: self.ttl.as_secs(),
                    };
                    let data = serde_json::to_vec(&new_lease)?;
                    self.store.put(LEADER_LEASE_KEY, &data).await?;
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
            None => {
                info!("No existing lease found, acquiring for {}", self.server_id);
                let lease = Lease {
                    id: "controller-leader".to_string(),
                    holder_id: self.server_id.clone(),
                    acquired_at: now,
                    renew_at: now,
                    ttl_seconds: self.ttl.as_secs(),
                };
                let data = serde_json::to_vec(&lease)?;
                self.store.put(LEADER_LEASE_KEY, &data).await?;
                Ok(true)
            }
        }
    }

    /// Start the leader election loop as a background task.
    pub fn start(self) -> (tokio::task::JoinHandle<()>, watch::Receiver<bool>) {
        let rx = self.leader_rx.clone();
        let handle = tokio::spawn(async move {
            info!(
                "LeaderElection started (server_id={}, ttl={}s, renew={}s)",
                self.server_id,
                self.ttl.as_secs(),
                self.renew_interval.as_secs()
            );

            let mut interval = tokio::time::interval(self.renew_interval);
            loop {
                interval.tick().await;

                match self.try_acquire_or_renew().await {
                    Ok(is_leader) => {
                        let was_leader = *self.leader_rx.borrow();
                        if is_leader && !was_leader {
                            info!("🏆 This server is now the LEADER ({})", self.server_id);
                        } else if !is_leader && was_leader {
                            warn!(
                                "⚠️  Leadership LOST for {} — another server took over",
                                self.server_id
                            );
                        }
                        let _ = self.leader_tx.send(is_leader);
                    }
                    Err(e) => {
                        warn!("Leader election error: {}", e);
                        let _ = self.leader_tx.send(false);
                    }
                }
            }
        });

        (handle, rx)
    }
}
