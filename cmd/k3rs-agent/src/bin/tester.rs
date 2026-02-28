fn main() {
    let json_str = r#"[{"id":"130812ab-a40a-4856-ab4c-f9b79946f3d0","name":"test-nginx-1284ca2c-130812ab","namespace":"default","spec":{"containers":[{"name":"nginx","image":"nginx:latest","command":[],"args":[],"env":{},"resources":{"cpu_millis":0,"memory_bytes":0},"volume_mounts":[]}],"node_affinity":{},"tolerations":[],"volumes":[]},"status":"Scheduled","status_message":null,"container_id":null,"node_name":"2539686e-d40c-4436-b049-35dc8d21db99","labels":{},"owner_ref":"f28d75ac-301a-479f-93a6-4dfe06c798cd","restart_count":0,"runtime_info":null,"created_at":"2026-02-28T07:52:54.118167508Z"}]"#;

    match serde_json::from_str::<serde_json::Value>(json_str) {
        Ok(v) => {
            println!(
                "Valid JSON structure: {:?}",
                v.get(0).map(|o| o.get("owner_ref"))
            );
        }
        Err(e) => eprintln!("Error: {}", e),
    }

    match serde_json::from_str::<Vec<pkg_types::pod::Pod>>(json_str) {
        Ok(_) => println!("Parsed strictly successfully!"),
        Err(e) => eprintln!("Strict parse error: {}", e),
    }
}
