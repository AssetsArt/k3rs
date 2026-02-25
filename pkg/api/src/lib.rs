pub mod auth;
pub mod handlers;
pub mod server;

use std::sync::Arc;

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
}
