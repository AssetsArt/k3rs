use clap::{Parser, Subcommand};
use futures_util::{SinkExt, StreamExt};
use pkg_types::configmap::ConfigMap;
use pkg_types::daemonset::DaemonSet;
use pkg_types::deployment::Deployment;
use pkg_types::hpa::HorizontalPodAutoscaler;
use pkg_types::job::{CronJob, Job};
use pkg_types::namespace::Namespace;
use pkg_types::node::{ClusterInfo, Node};
use pkg_types::pod::Pod;
use pkg_types::replicaset::ReplicaSet;
use pkg_types::secret::Secret;
use pkg_types::service::Service;
use tracing::info;

#[derive(Parser)]
#[command(name = "k3rsctl", about = "CLI tool for k3rs cluster management")]
struct Cli {
    /// Server API endpoint
    #[arg(long, default_value = "http://127.0.0.1:6443")]
    server: String,

    /// Authentication token
    #[arg(long, default_value = "demo-token-123")]
    token: String,

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
        /// Resource type (pods, services, deployments, configmaps, secrets, namespaces, replicasets, daemonsets, jobs, cronjobs, hpa)
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
    /// Delete a resource (by type/id or from a manifest file)
    Delete {
        /// Resource type (e.g. pods, deployments)
        resource: Option<String>,
        /// Resource ID or name
        id: Option<String>,
        /// Path to YAML/JSON manifest to delete
        #[arg(short, long)]
        file: Option<String>,
        /// Namespace
        #[arg(short, long, default_value = "default")]
        namespace: String,
    },
    /// Stream logs from a pod
    Logs {
        /// Pod ID
        pod_id: String,
        /// Namespace
        #[arg(short, long, default_value = "default")]
        namespace: String,
        /// Follow log output (poll every 2s)
        #[arg(short, long, default_value_t = false)]
        follow: bool,
    },
    /// Execute a command in a pod
    Exec {
        /// Pod ID
        pod_id: String,
        /// Command to execute (after --)
        #[arg(last = true)]
        command: Vec<String>,
        /// Namespace
        #[arg(short, long, default_value = "default")]
        namespace: String,
    },
    /// Manage container runtime
    Runtime {
        #[command(subcommand)]
        action: RuntimeAction,
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
    /// Drain a node (cordon + evict pods)
    Drain {
        /// Node name
        name: String,
    },
    /// Mark a node as unschedulable
    Cordon {
        /// Node name
        name: String,
    },
    /// Mark a node as schedulable again
    Uncordon {
        /// Node name
        name: String,
    },
}

#[derive(Subcommand)]
enum RuntimeAction {
    /// Show current container runtime info
    Info,
    /// Upgrade/download the latest container runtime (Linux only)
    Upgrade,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    let mut headers = reqwest::header::HeaderMap::new();
    let auth_value = format!("Bearer {}", cli.token);
    let mut auth_header = reqwest::header::HeaderValue::from_str(&auth_value)?;
    auth_header.set_sensitive(true);
    headers.insert(reqwest::header::AUTHORIZATION, auth_header);

    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .default_headers(headers)
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
                        pod.node_name.as_deref().unwrap_or("-")
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
                    "{:<38} {:<20} {:<12} {:<10} {:<10}",
                    "ID", "NAME", "NAMESPACE", "REPLICAS", "READY"
                );
                for d in &deploys {
                    println!(
                        "{:<38} {:<20} {:<12} {:<10} {:<10}",
                        d.id, d.name, d.namespace, d.spec.replicas, d.status.ready_replicas
                    );
                }
                if deploys.is_empty() {
                    println!("No deployments found in namespace '{}'", namespace);
                }
            }
            "replicasets" | "replicaset" | "rs" => {
                let url = format!("{}/api/v1/namespaces/{}/replicasets", base, namespace);
                let resp = client.get(&url).send().await?;
                let items: Vec<ReplicaSet> = resp.json().await?;
                println!(
                    "{:<38} {:<20} {:<12} {:<10} {:<10}",
                    "ID", "NAME", "NAMESPACE", "REPLICAS", "READY"
                );
                for rs in &items {
                    println!(
                        "{:<38} {:<20} {:<12} {:<10} {:<10}",
                        rs.id, rs.name, rs.namespace, rs.spec.replicas, rs.status.ready_replicas
                    );
                }
                if items.is_empty() {
                    println!("No replicasets found in namespace '{}'", namespace);
                }
            }
            "daemonsets" | "daemonset" | "ds" => {
                let url = format!("{}/api/v1/namespaces/{}/daemonsets", base, namespace);
                let resp = client.get(&url).send().await?;
                let items: Vec<DaemonSet> = resp.json().await?;
                println!(
                    "{:<38} {:<20} {:<12} {:<10} {:<10}",
                    "ID", "NAME", "NAMESPACE", "DESIRED", "READY"
                );
                for ds in &items {
                    println!(
                        "{:<38} {:<20} {:<12} {:<10} {:<10}",
                        ds.id,
                        ds.name,
                        ds.namespace,
                        ds.status.desired_number_scheduled,
                        ds.status.number_ready
                    );
                }
                if items.is_empty() {
                    println!("No daemonsets found in namespace '{}'", namespace);
                }
            }
            "jobs" | "job" => {
                let url = format!("{}/api/v1/namespaces/{}/jobs", base, namespace);
                let resp = client.get(&url).send().await?;
                let items: Vec<Job> = resp.json().await?;
                println!(
                    "{:<38} {:<20} {:<12} {:<10} {:<10}",
                    "ID", "NAME", "NAMESPACE", "STATUS", "SUCCEEDED"
                );
                for j in &items {
                    println!(
                        "{:<38} {:<20} {:<12} {:<10} {:<10}",
                        j.id, j.name, j.namespace, j.status.condition, j.status.succeeded
                    );
                }
                if items.is_empty() {
                    println!("No jobs found in namespace '{}'", namespace);
                }
            }
            "cronjobs" | "cronjob" | "cj" => {
                let url = format!("{}/api/v1/namespaces/{}/cronjobs", base, namespace);
                let resp = client.get(&url).send().await?;
                let items: Vec<CronJob> = resp.json().await?;
                println!(
                    "{:<38} {:<20} {:<12} {:<15} SUSPEND",
                    "ID", "NAME", "NAMESPACE", "SCHEDULE"
                );
                for cj in &items {
                    println!(
                        "{:<38} {:<20} {:<12} {:<15} {}",
                        cj.id, cj.name, cj.namespace, cj.spec.schedule, cj.spec.suspend
                    );
                }
                if items.is_empty() {
                    println!("No cronjobs found in namespace '{}'", namespace);
                }
            }
            "hpa" | "horizontalpodautoscalers" | "horizontalpodautoscaler" => {
                let url = format!("{}/api/v1/namespaces/{}/hpa", base, namespace);
                let resp = client.get(&url).send().await?;
                let items: Vec<HorizontalPodAutoscaler> = resp.json().await?;
                println!(
                    "{:<38} {:<20} {:<12} {:<8} {:<8} {:<10}",
                    "ID", "NAME", "NAMESPACE", "MIN", "MAX", "CURRENT"
                );
                for h in &items {
                    println!(
                        "{:<38} {:<20} {:<12} {:<8} {:<8} {:<10}",
                        h.id,
                        h.name,
                        h.namespace,
                        h.spec.min_replicas,
                        h.spec.max_replicas,
                        h.status.current_replicas
                    );
                }
                if items.is_empty() {
                    println!("No HPAs found in namespace '{}'", namespace);
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
                    "Unknown resource type: {}. Supported: pods, services, deployments, replicasets, daemonsets, jobs, cronjobs, hpa, configmaps, secrets, namespaces",
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
        }
        Commands::Delete {
            resource,
            id,
            file,
            namespace,
        } => {
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
        }
        Commands::Logs {
            pod_id,
            namespace,
            follow,
        } => {
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
        }
        Commands::Exec {
            pod_id,
            command,
            namespace,
        } => {
            // Connect via WebSocket to the exec endpoint
            let ws_url = cli
                .server
                .replace("http://", "ws://")
                .replace("https://", "wss://");
            let url = format!(
                "{}/api/v1/namespaces/{}/pods/{}/exec",
                ws_url, namespace, pod_id
            );

            let request = tokio_tungstenite::tungstenite::http::Request::builder()
                .uri(&url)
                .header("Authorization", format!("Bearer {}", cli.token))
                .header("Host", "localhost")
                .header("Connection", "Upgrade")
                .header("Upgrade", "websocket")
                .header("Sec-WebSocket-Version", "13")
                .header(
                    "Sec-WebSocket-Key",
                    tokio_tungstenite::tungstenite::handshake::client::generate_key(),
                )
                .body(())
                .expect("Failed to build WebSocket request");

            let (ws_stream, _) = match tokio_tungstenite::connect_async(request).await {
                Ok(conn) => conn,
                Err(e) => {
                    eprintln!("Failed to connect WebSocket: {}", e);
                    std::process::exit(1);
                }
            };

            let (mut write, mut read) = ws_stream.split();

            if command.is_empty() {
                // Interactive mode
                eprintln!("Entering interactive exec session. Type 'exit' to quit.");

                // Read the welcome message
                if let Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) =
                    read.next().await
                {
                    print!("{}", text);
                }

                let stdin = tokio::io::stdin();
                let reader = tokio::io::BufReader::new(stdin);
                let mut lines = tokio::io::AsyncBufReadExt::lines(reader);

                loop {
                    let line = match lines.next_line().await {
                        Ok(Some(line)) => line,
                        _ => break,
                    };

                    if line.trim() == "exit" {
                        let _ = write
                            .send(tokio_tungstenite::tungstenite::Message::Text("exit".into()))
                            .await;
                        break;
                    }

                    if write
                        .send(tokio_tungstenite::tungstenite::Message::Text(line.into()))
                        .await
                        .is_err()
                    {
                        break;
                    }

                    if let Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) =
                        read.next().await
                    {
                        print!("{}", text);
                    }
                }
            } else {
                // Non-interactive: send single command
                let cmd = command.join(" ");

                // Skip welcome message
                let _ = read.next().await;

                if write
                    .send(tokio_tungstenite::tungstenite::Message::Text(cmd.into()))
                    .await
                    .is_err()
                {
                    eprintln!("Failed to send command");
                    std::process::exit(1);
                }

                if let Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) =
                    read.next().await
                {
                    // Remove trailing prompt
                    let output = text
                        .trim_end()
                        .trim_end_matches("$ ")
                        .trim_end_matches("\r\n$ ");
                    println!("{}", output);
                }

                let _ = write
                    .send(tokio_tungstenite::tungstenite::Message::Text("exit".into()))
                    .await;
            }
        }
        Commands::Runtime { action } => match action {
            RuntimeAction::Info => {
                let resp = client
                    .get(format!("{}/api/v1/runtime", cli.server))
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
                    .put(format!("{}/api/v1/runtime/upgrade", cli.server))
                    .send()
                    .await?;
                let result: serde_json::Value = resp.json().await?;
                println!("Status: {}", result["status"].as_str().unwrap_or("unknown"));
                if let Some(msg) = result["message"].as_str() {
                    println!("Message: {}", msg);
                }
            }
        },
    }

    Ok(())
}
