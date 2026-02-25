use axum::Json;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct ProcessInfo {
    pub node_name: String,
    pub pid: u32,
    pub name: String,
    pub cpu_percent: f32,
    pub memory_bytes: u64,
}

/// List k3rs-related processes running on this node.
/// Requires two refresh cycles with a delay to get accurate CPU usage.
pub async fn list_processes() -> Json<Vec<ProcessInfo>> {
    use sysinfo::System;

    let mut sys = System::new_all();

    // First refresh — establishes baseline for CPU delta calculation
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

    // Sleep 250ms to allow CPU delta measurement
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;

    // Second refresh — now cpu_usage() returns real values
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

    let node_name = System::host_name().unwrap_or_else(|| "unknown".into());
    let mut procs: Vec<ProcessInfo> = sys
        .processes()
        .values()
        .filter(|p| {
            let name = p.name().to_string_lossy().to_lowercase();
            name.contains("k3rs")
        })
        .map(|p| ProcessInfo {
            node_name: node_name.clone(),
            pid: p.pid().as_u32(),
            name: p.name().to_string_lossy().to_string(),
            cpu_percent: p.cpu_usage(),
            memory_bytes: p.memory(),
        })
        .collect();

    // Sort by memory descending (heaviest first)
    procs.sort_by(|a, b| b.memory_bytes.cmp(&a.memory_bytes));

    Json(procs)
}
