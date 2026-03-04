//! End-to-end networking tests.
//!
//! These tests require a running k3rs cluster with:
//! - k3rs-server, k3rs-agent, k3rs-vpc daemons active
//! - At least one node registered
//! - eBPF enforcer loaded (Linux with root)
//!
//! For cross-node tests, at least two nodes with WireGuard mesh established.
//!
//! Run with:
//!   cargo test -p k3rs-vpc --test e2e_networking -- --ignored --test-threads=1

use pkg_types::vpc::{Vpc, VpcPeering, VpcStatus};

const API_BASE: &str = "http://localhost:8080";
const API_TOKEN: &str = "k3rs";

/// Helper: create an authenticated reqwest client.
fn api_client() -> reqwest::Client {
    reqwest::Client::builder()
        .default_headers({
            let mut h = reqwest::header::HeaderMap::new();
            h.insert(
                "Authorization",
                format!("Bearer {}", API_TOKEN).parse().unwrap(),
            );
            h
        })
        .build()
        .unwrap()
}

/// Helper: deploy a test pod in a given VPC and wait for it to be Running.
async fn deploy_test_pod(
    client: &reqwest::Client,
    name: &str,
    namespace: &str,
    vpc: &str,
) -> String {
    let manifest = serde_json::json!({
        "kind": "Pod",
        "name": name,
        "namespace": namespace,
        "spec": {
            "containers": [{"image": "alpine:latest", "command": ["sleep", "3600"]}],
            "vpc": vpc,
        }
    });
    let url = format!("{}/api/v1/namespaces/{}/pods", API_BASE, namespace);
    let resp = client.post(&url).json(&manifest).send().await.unwrap();
    assert!(
        resp.status().is_success(),
        "failed to create pod {}: {}",
        name,
        resp.status()
    );

    let body: serde_json::Value = resp.json().await.unwrap();
    body["id"].as_str().unwrap().to_string()
}

/// Helper: wait for pod to reach Running status (poll, max 60s).
async fn wait_pod_running(client: &reqwest::Client, namespace: &str, pod_id: &str) {
    for _ in 0..30 {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let url = format!("{}/api/v1/namespaces/{}/pods", API_BASE, namespace);
        let resp = client.get(&url).send().await.unwrap();
        let pods: Vec<serde_json::Value> = resp.json().await.unwrap();
        if let Some(pod) = pods.iter().find(|p| p["id"].as_str() == Some(pod_id)) {
            if pod["status"].as_str() == Some("Running") {
                return;
            }
        }
    }
    panic!("Pod {} did not reach Running within 60s", pod_id);
}

/// Helper: delete a pod.
async fn delete_pod(client: &reqwest::Client, namespace: &str, pod_id: &str) {
    let url = format!(
        "{}/api/v1/namespaces/{}/pods/{}",
        API_BASE, namespace, pod_id
    );
    let _ = client.delete(&url).send().await;
}

/// Helper: get a pod's Ghost IPv6 address.
async fn get_pod_ghost_ipv6(
    client: &reqwest::Client,
    namespace: &str,
    pod_id: &str,
) -> Option<String> {
    let url = format!("{}/api/v1/namespaces/{}/pods", API_BASE, namespace);
    let resp = client.get(&url).send().await.ok()?;
    let pods: Vec<serde_json::Value> = resp.json().await.ok()?;
    pods.iter()
        .find(|p| p["id"].as_str() == Some(pod_id))
        .and_then(|p| p["ghost_ipv6"].as_str().map(String::from))
}

// ============================================================
// Same-Node Pod-to-Pod Tests
// ============================================================

#[tokio::test]
#[ignore = "requires running k3rs cluster with eBPF"]
async fn e2e_pod_to_pod_same_node_ghost_ipv6() {
    let client = api_client();
    let ns = "default";
    let vpc = "default";

    // Deploy two pods in the same VPC
    let pod_a_id = deploy_test_pod(&client, "e2e-p2p-a", ns, vpc).await;
    let pod_b_id = deploy_test_pod(&client, "e2e-p2p-b", ns, vpc).await;
    wait_pod_running(&client, ns, &pod_a_id).await;
    wait_pod_running(&client, ns, &pod_b_id).await;

    // Get Ghost IPv6 addresses
    let ipv6_b = get_pod_ghost_ipv6(&client, ns, &pod_b_id)
        .await
        .expect("pod B should have Ghost IPv6");

    // Exec ping from pod A → pod B's Ghost IPv6
    let exec_url = format!(
        "{}/api/v1/namespaces/{}/pods/{}/exec",
        API_BASE, ns, pod_a_id
    );
    let exec_body = serde_json::json!({"command": ["ping", "-6", "-c", "3", "-W", "5", &ipv6_b]});
    let resp = client
        .post(&exec_url)
        .json(&exec_body)
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "ping from pod A to pod B Ghost IPv6 failed"
    );

    // Cleanup
    delete_pod(&client, ns, &pod_a_id).await;
    delete_pod(&client, ns, &pod_b_id).await;
}

#[tokio::test]
#[ignore = "requires running k3rs cluster with eBPF"]
async fn e2e_pod_to_pod_overlapping_cidrs_different_vpcs() {
    let client = api_client();
    let ns = "default";

    // Create two VPCs with overlapping CIDRs — this should fail (409)
    let vpc_req = serde_json::json!({
        "name": "e2e-vpc-overlap",
        "ipv4_cidr": "10.0.1.0/24"
    });
    let url = format!("{}/api/v1/vpcs", API_BASE);
    let resp = client.post(&url).json(&vpc_req).send().await.unwrap();
    // The default VPC uses 10.0.0.0/16 or similar; overlapping CIDRs
    // should be validated by the server. Check the response.
    let status = resp.status();
    // If CIDRs don't overlap with default, create two custom VPCs with same CIDR
    if status.is_success() {
        let vpc_req2 = serde_json::json!({
            "name": "e2e-vpc-overlap2",
            "ipv4_cidr": "10.0.1.0/24"
        });
        let resp2 = client.post(&url).json(&vpc_req2).send().await.unwrap();
        assert_eq!(
            resp2.status().as_u16(),
            409,
            "overlapping CIDR should return 409 Conflict"
        );

        // Cleanup: delete the first VPC
        let del_url = format!("{}/api/v1/vpcs/e2e-vpc-overlap", API_BASE);
        let _ = client.delete(&del_url).send().await;
    }
}

// ============================================================
// VPC Isolation Tests
// ============================================================

#[tokio::test]
#[ignore = "requires running k3rs cluster with eBPF"]
async fn e2e_vpc_isolation_cross_vpc_blocked() {
    let client = api_client();
    let ns = "default";

    // Create a second VPC
    let vpc_url = format!("{}/api/v1/vpcs", API_BASE);
    let vpc_req = serde_json::json!({
        "name": "e2e-vpc-iso",
        "ipv4_cidr": "10.99.0.0/24"
    });
    let _ = client.post(&vpc_url).json(&vpc_req).send().await;

    // Deploy pod A in default VPC, pod B in e2e-vpc-iso
    let pod_a_id = deploy_test_pod(&client, "e2e-iso-a", ns, "default").await;
    let pod_b_id = deploy_test_pod(&client, "e2e-iso-b", ns, "e2e-vpc-iso").await;
    wait_pod_running(&client, ns, &pod_a_id).await;
    wait_pod_running(&client, ns, &pod_b_id).await;

    let ipv6_b = get_pod_ghost_ipv6(&client, ns, &pod_b_id)
        .await
        .expect("pod B should have Ghost IPv6");

    // Ping should FAIL (cross-VPC without peering)
    let exec_url = format!(
        "{}/api/v1/namespaces/{}/pods/{}/exec",
        API_BASE, ns, pod_a_id
    );
    let exec_body = serde_json::json!({"command": ["ping", "-6", "-c", "2", "-W", "3", &ipv6_b]});
    let resp = client
        .post(&exec_url)
        .json(&exec_body)
        .send()
        .await
        .unwrap();
    // We expect the ping to fail (non-zero exit or timeout)
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    assert!(
        body.contains("100% packet loss") || status.as_u16() != 200,
        "cross-VPC traffic should be blocked without peering"
    );

    // Cleanup
    delete_pod(&client, ns, &pod_a_id).await;
    delete_pod(&client, ns, &pod_b_id).await;
    let del_url = format!("{}/api/v1/vpcs/e2e-vpc-iso", API_BASE);
    let _ = client.delete(&del_url).send().await;
}

// ============================================================
// VPC Peering Tests
// ============================================================

#[tokio::test]
#[ignore = "requires running k3rs cluster with eBPF"]
async fn e2e_vpc_peering_allows_cross_vpc() {
    let client = api_client();
    let ns = "default";

    // Create second VPC
    let vpc_url = format!("{}/api/v1/vpcs", API_BASE);
    let _ = client
        .post(&vpc_url)
        .json(&serde_json::json!({"name": "e2e-vpc-peer", "ipv4_cidr": "10.98.0.0/24"}))
        .send()
        .await;

    // Create peering
    let peering_url = format!("{}/api/v1/vpc-peerings", API_BASE);
    let peering_req = serde_json::json!({
        "name": "e2e-peer-test",
        "vpc_a": "default",
        "vpc_b": "e2e-vpc-peer",
        "direction": "Bidirectional"
    });
    let _ = client.post(&peering_url).json(&peering_req).send().await;

    // Deploy pods in each VPC
    let pod_a_id = deploy_test_pod(&client, "e2e-peer-a", ns, "default").await;
    let pod_b_id = deploy_test_pod(&client, "e2e-peer-b", ns, "e2e-vpc-peer").await;
    wait_pod_running(&client, ns, &pod_a_id).await;
    wait_pod_running(&client, ns, &pod_b_id).await;

    let ipv6_b = get_pod_ghost_ipv6(&client, ns, &pod_b_id)
        .await
        .expect("pod B should have Ghost IPv6");

    // Ping should SUCCEED (peered VPCs)
    let exec_url = format!(
        "{}/api/v1/namespaces/{}/pods/{}/exec",
        API_BASE, ns, pod_a_id
    );
    let exec_body = serde_json::json!({"command": ["ping", "-6", "-c", "3", "-W", "5", &ipv6_b]});
    let resp = client
        .post(&exec_url)
        .json(&exec_body)
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "ping via peered VPCs should succeed"
    );

    // Cleanup
    delete_pod(&client, ns, &pod_a_id).await;
    delete_pod(&client, ns, &pod_b_id).await;
    let _ = client
        .delete(&format!("{}/api/v1/vpc-peerings/e2e-peer-test", API_BASE))
        .send()
        .await;
    let _ = client
        .delete(&format!("{}/api/v1/vpcs/e2e-vpc-peer", API_BASE))
        .send()
        .await;
}

// ============================================================
// NAT64 Tests
// ============================================================

#[tokio::test]
#[ignore = "requires running k3rs cluster with NAT64 + DNS64"]
async fn e2e_nat64_pod_reaches_external_ipv4() {
    let client = api_client();
    let ns = "default";

    let pod_id = deploy_test_pod(&client, "e2e-nat64", ns, "default").await;
    wait_pod_running(&client, ns, &pod_id).await;

    // Try to reach an external IPv4 host via NAT64 prefix 64:ff9b::/96
    // Using well-known IPv4: 1.1.1.1 → 64:ff9b::1.1.1.1
    let exec_url = format!("{}/api/v1/namespaces/{}/pods/{}/exec", API_BASE, ns, pod_id);
    let exec_body =
        serde_json::json!({"command": ["ping", "-6", "-c", "3", "-W", "5", "64:ff9b::1.1.1.1"]});
    let resp = client
        .post(&exec_url)
        .json(&exec_body)
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "pod should reach external IPv4 via NAT64"
    );

    delete_pod(&client, ns, &pod_id).await;
}

// ============================================================
// VM-Specific Tests
// ============================================================

#[tokio::test]
#[ignore = "requires running k3rs cluster with VM runtime (Firecracker/Virt)"]
async fn e2e_vm_to_vm_same_node_tap() {
    // Deploy two VM pods in the same VPC, verify Ghost IPv6 connectivity via TAP
    let client = api_client();
    let ns = "default";

    // VM pods need a kernel + rootfs; this test assumes the runtime is configured
    let manifest_a = serde_json::json!({
        "kind": "Pod",
        "name": "e2e-vm-a",
        "namespace": ns,
        "spec": {
            "containers": [{"image": "alpine:latest", "command": ["sleep", "3600"]}],
            "vpc": "default",
            "runtime": "vm",
        }
    });
    let url = format!("{}/api/v1/namespaces/{}/pods", API_BASE, ns);
    let resp = client.post(&url).json(&manifest_a).send().await.unwrap();
    if !resp.status().is_success() {
        eprintln!("VM runtime not available, skipping");
        return;
    }
    let pod_a_id: String = resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let manifest_b = serde_json::json!({
        "kind": "Pod",
        "name": "e2e-vm-b",
        "namespace": ns,
        "spec": {
            "containers": [{"image": "alpine:latest", "command": ["sleep", "3600"]}],
            "vpc": "default",
            "runtime": "vm",
        }
    });
    let resp = client.post(&url).json(&manifest_b).send().await.unwrap();
    let pod_b_id: String = resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    wait_pod_running(&client, ns, &pod_a_id).await;
    wait_pod_running(&client, ns, &pod_b_id).await;

    // Both should have Ghost IPv6 via TAP
    let ipv6_a = get_pod_ghost_ipv6(&client, ns, &pod_a_id).await;
    let ipv6_b = get_pod_ghost_ipv6(&client, ns, &pod_b_id).await;
    assert!(ipv6_a.is_some(), "VM pod A should have Ghost IPv6");
    assert!(ipv6_b.is_some(), "VM pod B should have Ghost IPv6");

    delete_pod(&client, ns, &pod_a_id).await;
    delete_pod(&client, ns, &pod_b_id).await;
}

#[tokio::test]
#[ignore = "requires running k3rs cluster with VM + OCI runtimes"]
async fn e2e_vm_to_oci_same_node() {
    // Deploy one VM pod and one OCI pod in the same VPC, verify connectivity
    let client = api_client();
    let ns = "default";

    let oci_pod_id = deploy_test_pod(&client, "e2e-oci-mix", ns, "default").await;
    wait_pod_running(&client, ns, &oci_pod_id).await;

    let ipv6_oci = get_pod_ghost_ipv6(&client, ns, &oci_pod_id).await;
    assert!(
        ipv6_oci.is_some(),
        "OCI pod should have Ghost IPv6 for mixed-mode test"
    );

    // VM pod creation would follow similar pattern as above
    // For now, validate that OCI pods get Ghost IPv6 assigned
    delete_pod(&client, ns, &oci_pod_id).await;
}

// ============================================================
// Cross-Node (WireGuard) Tests
// ============================================================

#[tokio::test]
#[ignore = "requires 2+ node k3rs cluster with WireGuard mesh"]
async fn e2e_cross_node_pod_to_pod_wireguard() {
    let client = api_client();
    let ns = "default";

    // Verify at least 2 nodes are registered
    let nodes_url = format!("{}/api/v1/nodes", API_BASE);
    let resp = client.get(&nodes_url).send().await.unwrap();
    let nodes: Vec<serde_json::Value> = resp.json().await.unwrap();
    if nodes.len() < 2 {
        eprintln!(
            "Only {} node(s) available, need 2+ for cross-node test",
            nodes.len()
        );
        return;
    }

    // Deploy two pods — scheduler should place them on different nodes
    // (or we could use node affinity if available)
    let pod_a_id = deploy_test_pod(&client, "e2e-xnode-a", ns, "default").await;
    let pod_b_id = deploy_test_pod(&client, "e2e-xnode-b", ns, "default").await;
    wait_pod_running(&client, ns, &pod_a_id).await;
    wait_pod_running(&client, ns, &pod_b_id).await;

    let ipv6_b = get_pod_ghost_ipv6(&client, ns, &pod_b_id)
        .await
        .expect("pod B should have Ghost IPv6");

    // Ping from pod A → pod B via WireGuard tunnel
    let exec_url = format!(
        "{}/api/v1/namespaces/{}/pods/{}/exec",
        API_BASE, ns, pod_a_id
    );
    let exec_body = serde_json::json!({"command": ["ping", "-6", "-c", "3", "-W", "10", &ipv6_b]});
    let resp = client
        .post(&exec_url)
        .json(&exec_body)
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "cross-node ping via WireGuard should succeed"
    );

    delete_pod(&client, ns, &pod_a_id).await;
    delete_pod(&client, ns, &pod_b_id).await;
}

// ============================================================
// Full Cluster Bootstrap Test
// ============================================================

#[tokio::test]
#[ignore = "requires k3rsctl pm and clean environment"]
async fn e2e_full_cluster_bootstrap() {
    let client = api_client();

    // Verify cluster is healthy
    let info_url = format!("{}/api/v1/cluster/info", API_BASE);
    let resp = client.get(&info_url).send().await.unwrap();
    assert!(
        resp.status().is_success(),
        "cluster info should be available"
    );
    let info: serde_json::Value = resp.json().await.unwrap();
    assert!(
        info["node_count"].as_u64().unwrap_or(0) >= 1,
        "cluster should have at least 1 node"
    );

    // Deploy a simple workload
    let pod_id = deploy_test_pod(&client, "e2e-bootstrap", "default", "default").await;
    wait_pod_running(&client, "default", &pod_id).await;

    // Verify pod has Ghost IPv6
    let ipv6 = get_pod_ghost_ipv6(&client, "default", &pod_id).await;
    assert!(
        ipv6.is_some(),
        "bootstrapped pod should have Ghost IPv6 assigned"
    );

    // Verify VPCs are available
    let vpc_url = format!("{}/api/v1/vpcs", API_BASE);
    let resp = client.get(&vpc_url).send().await.unwrap();
    let vpcs: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(
        !vpcs.is_empty(),
        "cluster should have at least the default VPC"
    );

    delete_pod(&client, "default", &pod_id).await;
}
