use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A distributed lease for leader election.
/// Stored at `/registry/leases/<lease-id>` in SlateDB.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lease {
    /// Unique lease identifier (e.g. "controller-leader")
    pub id: String,
    /// The server instance holding this lease
    pub holder_id: String,
    /// When the lease was first acquired
    pub acquired_at: DateTime<Utc>,
    /// When the lease was last renewed
    pub renew_at: DateTime<Utc>,
    /// Lease time-to-live in seconds
    pub ttl_seconds: u64,
}

impl Lease {
    /// Check if this lease has expired.
    pub fn is_expired(&self) -> bool {
        let expiry = self.renew_at + chrono::Duration::seconds(self.ttl_seconds as i64);
        Utc::now() > expiry
    }
}
