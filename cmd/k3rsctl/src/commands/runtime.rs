use crate::cli::RuntimeAction;

pub async fn handle(
    client: &reqwest::Client,
    server: &str,
    action: &RuntimeAction,
) -> anyhow::Result<()> {
    match action {
        RuntimeAction::Info => {
            let resp = client
                .get(format!("{}/api/v1/runtime", server))
                .send()
                .await?;
            let info: serde_json::Value = resp.json().await?;
            println!("Container Runtime");
            println!(
                "  Backend:  {}",
                info["backend"].as_str().unwrap_or("unknown")
            );
            println!(
                "  Version:  {}",
                info["version"].as_str().unwrap_or("unknown")
            );
            println!("  OS:       {}", info["os"].as_str().unwrap_or("unknown"));
            println!("  Arch:     {}", info["arch"].as_str().unwrap_or("unknown"));
        }
        RuntimeAction::Upgrade => {
            println!("Upgrading container runtime...");
            let resp = client
                .put(format!("{}/api/v1/runtime/upgrade", server))
                .send()
                .await?;
            let result: serde_json::Value = resp.json().await?;
            println!("Status: {}", result["status"].as_str().unwrap_or("unknown"));
            if let Some(msg) = result["message"].as_str() {
                println!("Message: {}", msg);
            }
        }
    }
    Ok(())
}
