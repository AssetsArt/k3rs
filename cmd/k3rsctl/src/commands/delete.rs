pub async fn handle(
    client: &reqwest::Client,
    base: &str,
    resource: Option<&str>,
    id: Option<&str>,
    file: Option<&str>,
    namespace: &str,
) -> anyhow::Result<()> {
    if let Some(file_path) = file {
        // File-based delete: parse YAML to extract kind/name
        let content = tokio::fs::read_to_string(&file_path).await?;

        // Support multi-document YAML (--- separated)
        let mut deleted = 0;
        for doc in content.split("\n---") {
            let doc = doc.trim();
            if doc.is_empty() {
                continue;
            }

            let value: serde_yaml::Value = match serde_yaml::from_str(doc) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("Failed to parse YAML: {}", e);
                    continue;
                }
            };

            let kind = value
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown");
            let name = value
                .get("name")
                .and_then(|v| v.as_str())
                .or_else(|| {
                    value
                        .get("metadata")
                        .and_then(|m| m.get("name"))
                        .and_then(|v| v.as_str())
                })
                .unwrap_or("");
            let ns = value
                .get("namespace")
                .and_then(|v| v.as_str())
                .unwrap_or(namespace);

            if name.is_empty() {
                eprintln!("Skipping resource with no name in {}", file_path);
                continue;
            }

            // Map kind → API resource type
            let resource_type = match kind {
                "Pod" => "pods",
                "Service" => "services",
                "Deployment" => "deployments",
                "ReplicaSet" => "replicasets",
                "DaemonSet" => "daemonsets",
                "Job" => "jobs",
                "CronJob" => "cronjobs",
                "ConfigMap" => "configmaps",
                "Secret" => "secrets",
                "HorizontalPodAutoscaler" => "hpa",
                "Namespace" => "namespaces",
                "ResourceQuota" => "resourcequotas",
                "NetworkPolicy" => "networkpolicies",
                "PersistentVolumeClaim" => "pvcs",
                other => {
                    eprintln!("Unsupported resource kind for delete: {}", other);
                    continue;
                }
            };

            // Use the specific pod delete endpoint, or the generic one
            let url = if resource_type == "pods" {
                format!("{}/api/v1/namespaces/{}/pods/{}", base, ns, name)
            } else {
                format!("{}/api/v1/{}/{}/{}", base, resource_type, ns, name)
            };

            let resp = client.delete(&url).send().await?;
            if resp.status().is_success() {
                println!("{}/{} deleted", kind.to_lowercase(), name);
                deleted += 1;
            } else {
                eprintln!(
                    "Failed to delete {}/{}: {}",
                    kind.to_lowercase(),
                    name,
                    resp.status()
                );
            }
        }
        if deleted == 0 {
            eprintln!("No resources deleted from {}", file_path);
        }
    } else if let (Some(resource), Some(id)) = (resource, id) {
        // Positional args: delete <resource> <id>
        let url = format!("{}/api/v1/{}/{}/{}", base, resource, namespace, id);
        let resp = client.delete(&url).send().await?;
        if resp.status().is_success() {
            println!("{}/{} deleted", resource, id);
        } else {
            eprintln!("Failed to delete: {}", resp.status());
        }
    } else {
        eprintln!("Usage: k3rsctl delete <resource> <id> or k3rsctl delete -f <file>");
        std::process::exit(1);
    }
    Ok(())
}
