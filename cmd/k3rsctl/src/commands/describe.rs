use pkg_types::pod::Pod;

pub async fn handle(
    client: &reqwest::Client,
    base: &str,
    resource: &str,
    name: &str,
    namespace: &str,
) -> anyhow::Result<()> {
    match resource {
        "pod" | "pods" => describe_pod(client, base, name, namespace).await,
        other => {
            eprintln!(
                "Unknown resource type for describe: {}. Supported: pod",
                other
            );
            std::process::exit(1);
        }
    }
}

async fn describe_pod(
    client: &reqwest::Client,
    base: &str,
    name: &str,
    namespace: &str,
) -> anyhow::Result<()> {
    // Try by name first, then fall back to listing and matching
    let url = format!("{}/api/v1/namespaces/{}/pods", base, namespace);
    let resp = client.get(&url).send().await?;
    let pods: Vec<Pod> = resp.json().await?;

    let pod = pods.iter().find(|p| p.name == name || p.id == name);

    let Some(pod) = pod else {
        eprintln!("Pod '{}' not found in namespace '{}'", name, namespace);
        std::process::exit(1);
    };

    println!("Name:         {}", pod.name);
    println!("ID:           {}", pod.id);
    println!("Namespace:    {}", pod.namespace);
    println!("Status:       {}", pod.status);
    if let Some(ref msg) = pod.status_message {
        println!("Message:      {}", msg);
    }
    println!("Node:         {}", pod.node_name.as_deref().unwrap_or("-"));
    println!(
        "Created:      {}",
        pod.created_at.format("%Y-%m-%d %H:%M:%S")
    );

    // VPC info
    println!();
    println!(
        "VPC Name:     {}",
        pod.vpc_name
            .as_deref()
            .or(pod.spec.vpc.as_deref())
            .unwrap_or("default")
    );
    println!("Ghost IPv6:   {}", pod.ghost_ipv6.as_deref().unwrap_or("-"));

    // Container info
    println!();
    if let Some(ref cid) = pod.container_id {
        println!("Container ID: {}", cid);
    }
    if let Some(ref rt) = pod.runtime_info {
        println!("Runtime:      {} ({})", rt.backend, rt.version);
    }
    println!("Restarts:     {}", pod.restart_count);

    // Spec
    println!();
    println!("Containers:");
    for (i, c) in pod.spec.containers.iter().enumerate() {
        println!("  [{}] Image: {}", i, c.image);
        if !c.command.is_empty() {
            println!("      Command: {:?}", c.command);
        }
        if !c.args.is_empty() {
            println!("      Args: {:?}", c.args);
        }
    }

    if !pod.labels.is_empty() {
        println!();
        println!("Labels:");
        for (k, v) in &pod.labels {
            println!("  {}: {}", k, v);
        }
    }

    if let Some(ref owner) = pod.owner_ref {
        println!();
        println!("Owner:        {}", owner);
    }

    Ok(())
}
