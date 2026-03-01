pub mod auth;
pub mod handlers;
pub mod request_id;
pub mod server;

use std::sync::{Arc, atomic::AtomicBool};

use pkg_pki::ca::ClusterCA;
use pkg_scheduler::Scheduler;
use pkg_state::client::StateStore;

/// Shared application state injected into all Axum handlers.
#[derive(Clone)]
pub struct AppState {
    pub store: StateStore,
    pub ca: Arc<ClusterCA>,
    pub join_token: String,
    pub listen_addr: String,
    pub scheduler: Option<Arc<Scheduler>>,
    pub metrics: Arc<pkg_metrics::MetricsRegistry>,
    /// Directory for automated backup files (None = backups disabled).
    pub backup_dir: Option<String>,
    /// Set to true while a cluster restore is in progress (→ 503 on all routes).
    pub restore_in_progress: Arc<AtomicBool>,
    /// Set to true when this server holds the leader lease.
    pub is_leader: Arc<AtomicBool>,
}
