use crate::cli::ClusterAction;
use pkg_types::node::ClusterInfo;

pub async fn handle(
    client: &reqwest::Client,
    base: &str,
    action: &ClusterAction,
) -> anyhow::Result<()> {
    match action {
        ClusterAction::Info => {
            let url = format!("{}/api/v1/cluster/info", base);
            let resp = client.get(&url).send().await?;
            if !resp.status().is_success() {
                eprintln!("Error: server returned {}", resp.status());
                std::process::exit(1);
            }
            let info: ClusterInfo = resp.json().await?;
            println!("Cluster Endpoint:  {}", info.endpoint);
            println!("Version:           {}", info.version);
            println!("State Store:       {}", info.state_store);
            println!("Nodes:             {}", info.node_count);
        }
    }
    Ok(())
}
