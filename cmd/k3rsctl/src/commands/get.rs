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

pub async fn handle(
    client: &reqwest::Client,
    base: &str,
    resource: &str,
    namespace: &str,
) -> anyhow::Result<()> {
    match resource {
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
    }
    Ok(())
}
