use pkg_types::configmap::ConfigMap;
use pkg_types::daemonset::DaemonSet;
use pkg_types::deployment::Deployment;
use pkg_types::hpa::HorizontalPodAutoscaler;
use pkg_types::job::{CronJob, Job};
use pkg_types::namespace::Namespace;
use pkg_types::pod::Pod;
use pkg_types::replicaset::ReplicaSet;
use pkg_types::secret::Secret;
use pkg_types::service::Service;
use tracing::info;

pub async fn handle(
    client: &reqwest::Client,
    base: &str,
    file: &str,
    namespace: &str,
) -> anyhow::Result<()> {
    info!("Applying manifest from {}", file);
    let content = tokio::fs::read_to_string(file).await?;

    // Parse as a generic YAML value to detect the kind
    let value: serde_yaml::Value = serde_yaml::from_str(&content)?;
    let kind = value.get("kind").and_then(|v| v.as_str()).unwrap_or("Pod");

    match kind {
        "Pod" => {
            let pod: Pod = serde_yaml::from_str(&content)?;
            let url = format!("{}/api/v1/namespaces/{}/pods", base, namespace);
            let resp = client.post(&url).json(&pod).send().await?;
            if resp.status().is_success() {
                let created: Pod = resp.json().await?;
                println!("pod/{} created (id={})", created.name, created.id);
            } else {
                eprintln!("Failed to apply: {}", resp.status());
            }
        }
        "Namespace" => {
            let ns: Namespace = serde_yaml::from_str(&content)?;
            let url = format!("{}/api/v1/namespaces", base);
            let resp = client.post(&url).json(&ns).send().await?;
            if resp.status().is_success() {
                let created: Namespace = resp.json().await?;
                println!("namespace/{} created", created.name);
            } else {
                eprintln!("Failed to apply: {}", resp.status());
            }
        }
        "Service" => {
            let svc: Service = serde_yaml::from_str(&content)?;
            let url = format!("{}/api/v1/namespaces/{}/services", base, namespace);
            let resp = client.post(&url).json(&svc).send().await?;
            if resp.status().is_success() {
                let created: Service = resp.json().await?;
                println!("service/{} created (id={})", created.name, created.id);
            } else {
                eprintln!("Failed to apply: {}", resp.status());
            }
        }
        "Deployment" => {
            let deploy: Deployment = serde_yaml::from_str(&content)?;
            let url = format!("{}/api/v1/namespaces/{}/deployments", base, namespace);
            let resp = client.post(&url).json(&deploy).send().await?;
            if resp.status().is_success() {
                let created: Deployment = resp.json().await?;
                println!("deployment/{} created (id={})", created.name, created.id);
            } else {
                eprintln!("Failed to apply: {}", resp.status());
            }
        }
        "ReplicaSet" => {
            let rs: ReplicaSet = serde_yaml::from_str(&content)?;
            let url = format!("{}/api/v1/namespaces/{}/replicasets", base, namespace);
            let resp = client.post(&url).json(&rs).send().await?;
            if resp.status().is_success() {
                let created: ReplicaSet = resp.json().await?;
                println!("replicaset/{} created (id={})", created.name, created.id);
            } else {
                eprintln!("Failed to apply: {}", resp.status());
            }
        }
        "DaemonSet" => {
            let ds: DaemonSet = serde_yaml::from_str(&content)?;
            let url = format!("{}/api/v1/namespaces/{}/daemonsets", base, namespace);
            let resp = client.post(&url).json(&ds).send().await?;
            if resp.status().is_success() {
                let created: DaemonSet = resp.json().await?;
                println!("daemonset/{} created (id={})", created.name, created.id);
            } else {
                eprintln!("Failed to apply: {}", resp.status());
            }
        }
        "Job" => {
            let job: Job = serde_yaml::from_str(&content)?;
            let url = format!("{}/api/v1/namespaces/{}/jobs", base, namespace);
            let resp = client.post(&url).json(&job).send().await?;
            if resp.status().is_success() {
                let created: Job = resp.json().await?;
                println!("job/{} created (id={})", created.name, created.id);
            } else {
                eprintln!("Failed to apply: {}", resp.status());
            }
        }
        "CronJob" => {
            let cj: CronJob = serde_yaml::from_str(&content)?;
            let url = format!("{}/api/v1/namespaces/{}/cronjobs", base, namespace);
            let resp = client.post(&url).json(&cj).send().await?;
            if resp.status().is_success() {
                let created: CronJob = resp.json().await?;
                println!("cronjob/{} created (id={})", created.name, created.id);
            } else {
                eprintln!("Failed to apply: {}", resp.status());
            }
        }
        "HorizontalPodAutoscaler" => {
            let hpa: HorizontalPodAutoscaler = serde_yaml::from_str(&content)?;
            let url = format!("{}/api/v1/namespaces/{}/hpa", base, namespace);
            let resp = client.post(&url).json(&hpa).send().await?;
            if resp.status().is_success() {
                let created: HorizontalPodAutoscaler = resp.json().await?;
                println!("hpa/{} created (id={})", created.name, created.id);
            } else {
                eprintln!("Failed to apply: {}", resp.status());
            }
        }
        "ConfigMap" => {
            let cm: ConfigMap = serde_yaml::from_str(&content)?;
            let url = format!("{}/api/v1/namespaces/{}/configmaps", base, namespace);
            let resp = client.post(&url).json(&cm).send().await?;
            if resp.status().is_success() {
                let created: ConfigMap = resp.json().await?;
                println!("configmap/{} created (id={})", created.name, created.id);
            } else {
                eprintln!("Failed to apply: {}", resp.status());
            }
        }
        "Secret" => {
            let secret: Secret = serde_yaml::from_str(&content)?;
            let url = format!("{}/api/v1/namespaces/{}/secrets", base, namespace);
            let resp = client.post(&url).json(&secret).send().await?;
            if resp.status().is_success() {
                let created: Secret = resp.json().await?;
                println!("secret/{} created (id={})", created.name, created.id);
            } else {
                eprintln!("Failed to apply: {}", resp.status());
            }
        }
        other => {
            eprintln!("Unsupported resource kind: {}", other);
            std::process::exit(1);
        }
    }
    Ok(())
}
