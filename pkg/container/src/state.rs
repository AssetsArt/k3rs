//! In-process container state tracking.
//!
//! Tracks the lifecycle of every container managed by this runtime instance.
//! No mocking — every entry represents a real container created via the OCI backend.

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use std::sync::Arc;

/// Container lifecycle states.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ContainerState {
    /// Container has been created but not yet started.
    Created,
    /// Container is actively running.
    Running,
    /// Container stopped normally.
    Stopped,
    /// Container failed with an error message.
    Failed(String),
}

impl std::fmt::Display for ContainerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ContainerState::Created => write!(f, "created"),
            ContainerState::Running => write!(f, "running"),
            ContainerState::Stopped => write!(f, "stopped"),
            ContainerState::Failed(reason) => write!(f, "failed: {}", reason),
        }
    }
}

/// Tracked metadata for a single container.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ContainerEntry {
    /// Container ID (matches OCI container ID).
    pub id: String,
    /// OCI image reference used to create this container.
    pub image: String,
    /// Backend name used for this container.
    pub runtime_name: String,
    /// Current lifecycle state.
    pub state: ContainerState,
    /// Container PID (from the OCI runtime), if running.
    pub pid: Option<u32>,
    /// Exit code, if the container has stopped.
    pub exit_code: Option<i32>,
    /// Path to the container's stdout/stderr log file.
    pub log_path: String,
    /// Path to the OCI bundle directory.
    pub bundle_path: String,
    /// When the container entry was created.
    pub created_at: DateTime<Utc>,
    /// When the container was started (transitioned to Running).
    pub started_at: Option<DateTime<Utc>>,
    /// When the container finished (transitioned to Stopped/Failed).
    pub finished_at: Option<DateTime<Utc>>,
}

/// Concurrent in-memory container state store.
///
/// Thread-safe via `DashMap` — supports concurrent reads/writes from
/// the pod sync loop, log queries, and health checks.
#[derive(Debug, Clone)]
pub struct ContainerStore {
    containers: Arc<DashMap<String, ContainerEntry>>,
}

impl ContainerStore {
    /// Create an empty container store.
    pub fn new() -> Self {
        Self {
            containers: Arc::new(DashMap::new()),
        }
    }

    /// Register a new container after `backend.create()` succeeds.
    pub fn track(&self, id: &str, image: &str, runtime_name: &str, bundle_path: &str, log_path: &str) {
        let entry = ContainerEntry {
            id: id.to_string(),
            image: image.to_string(),
            runtime_name: runtime_name.to_string(),
            state: ContainerState::Created,
            pid: None,
            exit_code: None,
            log_path: log_path.to_string(),
            bundle_path: bundle_path.to_string(),
            created_at: Utc::now(),
            started_at: None,
            finished_at: None,
        };
        self.containers.insert(id.to_string(), entry);
    }

    /// Update a container's state. Returns `false` if the container is not tracked.
    pub fn update_state(&self, id: &str, state: ContainerState) -> bool {
        if let Some(mut entry) = self.containers.get_mut(id) {
            match &state {
                ContainerState::Running => {
                    entry.started_at = Some(Utc::now());
                }
                ContainerState::Stopped | ContainerState::Failed(_) => {
                    entry.finished_at = Some(Utc::now());
                }
                _ => {}
            }
            entry.state = state;
            true
        } else {
            false
        }
    }

    /// Set the PID for a running container.
    pub fn set_pid(&self, id: &str, pid: u32) {
        if let Some(mut entry) = self.containers.get_mut(id) {
            entry.pid = Some(pid);
        }
    }

    /// Set the exit code for a stopped container.
    pub fn set_exit_code(&self, id: &str, code: i32) {
        if let Some(mut entry) = self.containers.get_mut(id) {
            entry.exit_code = Some(code);
        }
    }

    /// Get a snapshot of a container's entry.
    pub fn get(&self, id: &str) -> Option<ContainerEntry> {
        self.containers.get(id).map(|e| e.clone())
    }

    /// List all tracked containers.
    pub fn list(&self) -> Vec<ContainerEntry> {
        self.containers.iter().map(|e| e.value().clone()).collect()
    }

    /// Remove a container from tracking (after cleanup).
    pub fn remove(&self, id: &str) -> Option<ContainerEntry> {
        self.containers.remove(id).map(|(_, e)| e)
    }

    /// Number of tracked containers.
    pub fn len(&self) -> usize {
        self.containers.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.containers.is_empty()
    }
}

impl Default for ContainerStore {
    fn default() -> Self {
        Self::new()
    }
}

// ─── OCI Runtime State Query ───────────────────────────────────

/// Parsed output from `<runtime> state <container_id>`.
/// See: https://github.com/opencontainers/runtime-spec/blob/main/runtime.md#state
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ContainerStateInfo {
    /// Container ID.
    pub id: String,
    /// OCI state: "creating", "created", "running", "stopped".
    pub status: String,
    /// Container PID (0 if not running).
    #[serde(default)]
    pub pid: u32,
    /// Bundle path.
    #[serde(default)]
    pub bundle: String,
}

// ─── Tests ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_track_and_get() {
        let store = ContainerStore::new();
        store.track("c1", "alpine:latest", "youki", "/tmp/bundle/c1", "/tmp/logs/c1.log");

        let entry = store.get("c1").expect("should find container");
        assert_eq!(entry.id, "c1");
        assert_eq!(entry.image, "alpine:latest");
        assert_eq!(entry.runtime_name, "youki");
        assert_eq!(entry.state, ContainerState::Created);
        assert!(entry.pid.is_none());
        assert!(entry.started_at.is_none());
    }

    #[test]
    fn test_state_transitions() {
        let store = ContainerStore::new();
        store.track("c2", "nginx:latest", "crun", "/tmp/bundle/c2", "/tmp/logs/c2.log");

        // Created → Running
        assert!(store.update_state("c2", ContainerState::Running));
        let entry = store.get("c2").unwrap();
        assert_eq!(entry.state, ContainerState::Running);
        assert!(entry.started_at.is_some());

        // Running → Stopped
        assert!(store.update_state("c2", ContainerState::Stopped));
        let entry = store.get("c2").unwrap();
        assert_eq!(entry.state, ContainerState::Stopped);
        assert!(entry.finished_at.is_some());
    }

    #[test]
    fn test_failed_state() {
        let store = ContainerStore::new();
        store.track("c3", "bad:image", "youki", "/tmp/bundle/c3", "/tmp/logs/c3.log");

        let reason = "OCI runtime error: exec format error".to_string();
        assert!(store.update_state("c3", ContainerState::Failed(reason.clone())));

        let entry = store.get("c3").unwrap();
        assert_eq!(entry.state, ContainerState::Failed(reason));
        assert!(entry.finished_at.is_some());
    }

    #[test]
    fn test_pid_and_exit_code() {
        let store = ContainerStore::new();
        store.track("c4", "alpine:latest", "youki", "/tmp/bundle/c4", "/tmp/logs/c4.log");

        store.set_pid("c4", 12345);
        assert_eq!(store.get("c4").unwrap().pid, Some(12345));

        store.set_exit_code("c4", 0);
        assert_eq!(store.get("c4").unwrap().exit_code, Some(0));
    }

    #[test]
    fn test_remove() {
        let store = ContainerStore::new();
        store.track("c5", "alpine:latest", "youki", "/tmp/bundle/c5", "/tmp/logs/c5.log");
        assert_eq!(store.len(), 1);

        let removed = store.remove("c5");
        assert!(removed.is_some());
        assert_eq!(store.len(), 0);
        assert!(store.get("c5").is_none());
    }

    #[test]
    fn test_list() {
        let store = ContainerStore::new();
        store.track("a1", "img1", "youki", "/b/a1", "/l/a1");
        store.track("a2", "img2", "crun", "/b/a2", "/l/a2");
        store.track("a3", "img3", "youki", "/b/a3", "/l/a3");

        let all = store.list();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn test_update_nonexistent() {
        let store = ContainerStore::new();
        assert!(!store.update_state("nope", ContainerState::Running));
    }
}
