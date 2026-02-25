use clap::{Parser, Subcommand};
use pkg_types::configmap::ConfigMap;
use pkg_types::deployment::Deployment;
use pkg_types::namespace::Namespace;
use pkg_types::node::{ClusterInfo, Node};
use pkg_types::pod::Pod;
use pkg_types::secret::Secret;
use pkg_types::service::Service;
use tracing::info;

#[derive(Parser)]
#[command(name = "k3rsctl", about = "CLI tool for k3rs cluster management")]
struct Cli {
    /// Server API endpoint
    #[arg(long, default_value = "http://127.0.0.1:6443")]
    server: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Show cluster information
    Cluster {
        #[command(subcommand)]
        action: ClusterAction,
    },
    /// Manage nodes
    Node {
        #[command(subcommand)]
        action: NodeAction,
    },
    /// Get resources
    Get {
        /// Resource type (pods, services, deployments, configmaps, secrets, namespaces)
        resource: String,
        /// Namespace (default: "default")
        #[arg(short, long, default_value = "default")]
        namespace: String,
    },
    /// Apply a manifest file
    Apply {
        /// Path to YAML/JSON manifest
        #[arg(short, long)]
        file: String,
        /// Namespace (default: "default")
        #[arg(short, long, default_value = "default")]
        namespace: String,
    },
    /// Delete a resource
    Delete {
        /// Resource type
        resource: String,
        /// Resource ID
        id: String,
        /// Namespace
        #[arg(short, long, default_value = "default")]
        namespace: String,
    },
}

#[derive(Subcommand)]
enum ClusterAction {
    /// Display cluster info
    Info,
}

#[derive(Subcommand)]
enum NodeAction {
    /// List all registered nodes
    List,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()?;

    let base = cli.server.trim_end_matches('/');

    match &cli.command {
        Commands::Cluster { action } => match action {
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
        },
        Commands::Node { action } => match action {
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
        },
        Commands::Get {
            resource,
            namespace,
        } => match resource.as_str() {
            "pods" | "pod" => {
                let url = format!("{}/api/v1/namespaces/{}/pods", base, namespace);
                let resp = client.get(&url).send().await?;
                let pods: Vec<Pod> = resp.json().await?;
                println!(
                    "{:<38} {:<20} {:<12} {:<10} NODE",
                    "ID", "NAME", "NAMESPACE", "STATUS"
                );
                for pod in &pods {
                    println!(
                        "{:<38} {:<20} {:<12} {:<10} {}",
                        pod.id,
                        pod.name,
                        pod.namespace,
                        pod.status,
                        pod.node_id.as_deref().unwrap_or("-")
                    );
                }
                if pods.is_empty() {
                    println!("No pods found in namespace '{}'", namespace);
                }
            }
            "services" | "service" | "svc" => {
                let url = format!("{}/api/v1/namespaces/{}/services", base, namespace);
                let resp = client.get(&url).send().await?;
                let svcs: Vec<Service> = resp.json().await?;
                println!(
                    "{:<38} {:<20} {:<12} {:<14} CLUSTER-IP",
                    "ID", "NAME", "NAMESPACE", "TYPE"
                );
                for svc in &svcs {
                    println!(
                        "{:<38} {:<20} {:<12} {:<14} {}",
                        svc.id,
                        svc.name,
                        svc.namespace,
                        svc.spec.service_type,
                        svc.cluster_ip.as_deref().unwrap_or("-")
                    );
                }
                if svcs.is_empty() {
                    println!("No services found in namespace '{}'", namespace);
                }
            }
            "deployments" | "deployment" | "deploy" => {
                let url = format!("{}/api/v1/namespaces/{}/deployments", base, namespace);
                let resp = client.get(&url).send().await?;
                let deploys: Vec<Deployment> = resp.json().await?;
                println!(
                    "{:<38} {:<20} {:<12} {:<10}",
                    "ID", "NAME", "NAMESPACE", "REPLICAS"
                );
                for d in &deploys {
                    println!(
                        "{:<38} {:<20} {:<12} {:<10}",
                        d.id, d.name, d.namespace, d.spec.replicas
                    );
                }
                if deploys.is_empty() {
                    println!("No deployments found in namespace '{}'", namespace);
                }
            }
            "configmaps" | "configmap" | "cm" => {
                let url = format!("{}/api/v1/namespaces/{}/configmaps", base, namespace);
                let resp = client.get(&url).send().await?;
                let cms: Vec<ConfigMap> = resp.json().await?;
                println!("{:<38} {:<20} {:<12} KEYS", "ID", "NAME", "NAMESPACE");
                for cm in &cms {
                    println!(
                        "{:<38} {:<20} {:<12} {}",
                        cm.id,
                        cm.name,
                        cm.namespace,
                        cm.data.len()
                    );
                }
                if cms.is_empty() {
                    println!("No configmaps found in namespace '{}'", namespace);
                }
            }
            "secrets" | "secret" => {
                let url = format!("{}/api/v1/namespaces/{}/secrets", base, namespace);
                let resp = client.get(&url).send().await?;
                let secrets: Vec<Secret> = resp.json().await?;
                println!("{:<38} {:<20} {:<12} KEYS", "ID", "NAME", "NAMESPACE");
                for s in &secrets {
                    println!(
                        "{:<38} {:<20} {:<12} {}",
                        s.id,
                        s.name,
                        s.namespace,
                        s.data.len()
                    );
                }
                if secrets.is_empty() {
                    println!("No secrets found in namespace '{}'", namespace);
                }
            }
            "namespaces" | "namespace" | "ns" => {
                let url = format!("{}/api/v1/namespaces", base);
                let resp = client.get(&url).send().await?;
                let nss: Vec<Namespace> = resp.json().await?;
                println!("{:<20} CREATED", "NAME");
                for ns in &nss {
                    println!(
                        "{:<20} {}",
                        ns.name,
                        ns.created_at.format("%Y-%m-%d %H:%M:%S")
                    );
                }
            }
            other => {
                eprintln!(
                    "Unknown resource type: {}. Supported: pods, services, deployments, configmaps, secrets, namespaces",
                    other
                );
                std::process::exit(1);
            }
        },
        Commands::Apply { file, namespace } => {
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
        }
        Commands::Delete {
            resource,
            id,
            namespace,
        } => {
            let url = format!("{}/api/v1/{}/{}/{}", base, resource, namespace, id);
            let resp = client.delete(&url).send().await?;
            if resp.status().is_success() {
                println!("{}/{} deleted", resource, id);
            } else {
                eprintln!("Failed to delete: {}", resp.status());
            }
        }
    }

    Ok(())
}
