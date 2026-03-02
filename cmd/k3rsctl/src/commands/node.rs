use crate::cli::NodeAction;
use pkg_types::node::Node;

pub async fn handle(
    client: &reqwest::Client,
    base: &str,
    action: &NodeAction,
) -> anyhow::Result<()> {
    match action {
        NodeAction::List => {
            let url = format!("{}/api/v1/nodes", base);
            let resp = client.get(&url).send().await?;
            if !resp.status().is_success() {
                eprintln!("Error: server returned {}", resp.status());
                std::process::exit(1);
            }
            let nodes: Vec<Node> = resp.json().await?;
            println!("{:<38} {:<16} {:<10} REGISTERED", "ID", "NAME", "STATUS");
            for node in &nodes {
                println!(
                    "{:<38} {:<16} {:<10} {}",
                    node.id,
                    node.name,
                    node.status,
                    node.registered_at.format("%Y-%m-%d %H:%M:%S")
                );
            }
            if nodes.is_empty() {
                println!("(no nodes registered)");
            }
        }
        NodeAction::Drain { name } => {
            let url = format!("{}/api/v1/nodes/{}/drain", base, name);
            let resp = client.post(&url).send().await?;
            if resp.status().is_success() {
                let body: serde_json::Value = resp.json().await?;
                println!(
                    "Node {} drained ({} pods evicted)",
                    name,
                    body.get("evicted_pods")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0)
                );
            } else {
                eprintln!("Failed to drain node {}: {}", name, resp.status());
                if let Ok(text) = resp.text().await {
                    eprintln!("  {}", text);
                }
            }
        }
        NodeAction::Cordon { name } => {
            let url = format!("{}/api/v1/nodes/{}/cordon", base, name);
            let resp = client.post(&url).send().await?;
            if resp.status().is_success() {
                println!("Node {} cordoned", name);
            } else {
                eprintln!("Failed to cordon node {}: {}", name, resp.status());
            }
        }
        NodeAction::Uncordon { name } => {
            let url = format!("{}/api/v1/nodes/{}/uncordon", base, name);
            let resp = client.post(&url).send().await?;
            if resp.status().is_success() {
                println!("Node {} uncordoned", name);
            } else {
                eprintln!("Failed to uncordon node {}: {}", name, resp.status());
            }
        }
    }
    Ok(())
}
