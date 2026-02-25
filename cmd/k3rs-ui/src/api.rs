use crate::*;

/// Server configuration — the k3rs API server address.
const K3RS_API: &str = "http://127.0.0.1:6443";
const K3RS_TOKEN: &str = "demo-token-123";

// ============================================================
// Server functions — run on the server, called from WASM client
// ============================================================

#[get("/api/ui/cluster-info")]
pub async fn get_cluster_info() -> Result<ClusterInfo> {
    let url = format!("{}/api/v1/cluster/info", K3RS_API);
    let resp = reqwest::Client::new()
        .get(&url)
        .header("Authorization", format!("Bearer {}", K3RS_TOKEN))
        .send()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    let info: ClusterInfo = resp
        .json()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    Ok(info)
}

#[get("/api/ui/nodes")]
pub async fn get_nodes() -> Result<Vec<Node>> {
    let url = format!("{}/api/v1/nodes", K3RS_API);
    let resp = reqwest::Client::new()
        .get(&url)
        .header("Authorization", format!("Bearer {}", K3RS_TOKEN))
        .send()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    let nodes: Vec<Node> = resp
        .json()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    Ok(nodes)
}

#[get("/api/ui/pods?ns")]
pub async fn get_pods(ns: String) -> Result<Vec<Pod>> {
    let url = format!("{}/api/v1/namespaces/{}/pods", K3RS_API, ns);
    let resp = reqwest::Client::new()
        .get(&url)
        .header("Authorization", format!("Bearer {}", K3RS_TOKEN))
        .send()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    let pods: Vec<Pod> = resp
        .json()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    Ok(pods)
}

#[get("/api/ui/services?ns")]
pub async fn get_services(ns: String) -> Result<Vec<Service>> {
    let url = format!("{}/api/v1/namespaces/{}/services", K3RS_API, ns);
    let resp = reqwest::Client::new()
        .get(&url)
        .header("Authorization", format!("Bearer {}", K3RS_TOKEN))
        .send()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    let svcs: Vec<Service> = resp
        .json()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    Ok(svcs)
}

#[get("/api/ui/deployments?ns")]
pub async fn get_deployments(ns: String) -> Result<Vec<Deployment>> {
    let url = format!("{}/api/v1/namespaces/{}/deployments", K3RS_API, ns);
    let resp = reqwest::Client::new()
        .get(&url)
        .header("Authorization", format!("Bearer {}", K3RS_TOKEN))
        .send()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    let deps: Vec<Deployment> = resp
        .json()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    Ok(deps)
}

#[get("/api/ui/configmaps?ns")]
pub async fn get_configmaps(ns: String) -> Result<Vec<ConfigMap>> {
    let url = format!("{}/api/v1/namespaces/{}/configmaps", K3RS_API, ns);
    let resp = reqwest::Client::new()
        .get(&url)
        .header("Authorization", format!("Bearer {}", K3RS_TOKEN))
        .send()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    let cms: Vec<ConfigMap> = resp
        .json()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    Ok(cms)
}

#[get("/api/ui/secrets?ns")]
pub async fn get_secrets(ns: String) -> Result<Vec<Secret>> {
    let url = format!("{}/api/v1/namespaces/{}/secrets", K3RS_API, ns);
    let resp = reqwest::Client::new()
        .get(&url)
        .header("Authorization", format!("Bearer {}", K3RS_TOKEN))
        .send()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    let secrets: Vec<Secret> = resp
        .json()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    Ok(secrets)
}

#[get("/api/ui/ingresses?ns")]
pub async fn get_ingresses(ns: String) -> Result<Vec<IngressObj>> {
    let url = format!("{}/api/v1/namespaces/{}/ingresses", K3RS_API, ns);
    let resp = reqwest::Client::new()
        .get(&url)
        .header("Authorization", format!("Bearer {}", K3RS_TOKEN))
        .send()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    let ings: Vec<IngressObj> = resp
        .json()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    Ok(ings)
}

#[get("/api/ui/quotas?ns")]
pub async fn get_quotas(ns: String) -> Result<Vec<ResourceQuota>> {
    let url = format!("{}/api/v1/namespaces/{}/resourcequotas", K3RS_API, ns);
    let resp = reqwest::Client::new()
        .get(&url)
        .header("Authorization", format!("Bearer {}", K3RS_TOKEN))
        .send()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    let items: Vec<ResourceQuota> = resp
        .json()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    Ok(items)
}

#[get("/api/ui/network-policies?ns")]
pub async fn get_network_policies(ns: String) -> Result<Vec<NetworkPolicyObj>> {
    let url = format!("{}/api/v1/namespaces/{}/networkpolicies", K3RS_API, ns);
    let resp = reqwest::Client::new()
        .get(&url)
        .header("Authorization", format!("Bearer {}", K3RS_TOKEN))
        .send()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    let items: Vec<NetworkPolicyObj> = resp
        .json()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    Ok(items)
}

#[get("/api/ui/pvcs?ns")]
pub async fn get_pvcs(ns: String) -> Result<Vec<PVC>> {
    let url = format!("{}/api/v1/namespaces/{}/pvcs", K3RS_API, ns);
    let resp = reqwest::Client::new()
        .get(&url)
        .header("Authorization", format!("Bearer {}", K3RS_TOKEN))
        .send()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    let items: Vec<PVC> = resp
        .json()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    Ok(items)
}

#[get("/api/ui/metrics")]
pub async fn get_metrics() -> Result<String> {
    let url = format!("{}/metrics", K3RS_API);
    let resp = reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    let text = resp
        .text()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;
    Ok(text)
}
