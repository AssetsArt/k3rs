pub async fn handle(
    client: &reqwest::Client,
    base: &str,
    pod_id: &str,
    namespace: &str,
    follow: bool,
) -> anyhow::Result<()> {
    let url = format!(
        "{}/api/v1/namespaces/{}/pods/{}/logs",
        base, namespace, pod_id
    );

    loop {
        let resp = client.get(&url).send().await?;
        if resp.status().is_success() {
            let body: serde_json::Value = resp.json().await?;
            if let Some(logs) = body.get("logs").and_then(|l| l.as_array()) {
                for line in logs {
                    println!("{}", line.as_str().unwrap_or(""));
                }
            }
        } else if resp.status().as_u16() == 404 {
            eprintln!("Pod {} not found in namespace {}", pod_id, namespace);
            break;
        } else {
            eprintln!("Failed to get logs: {}", resp.status());
            break;
        }

        if !follow {
            break;
        }

        // Poll every 2 seconds in follow mode
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    Ok(())
}
