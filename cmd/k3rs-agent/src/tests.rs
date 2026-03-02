//! Unit tests for k3rs-agent — Group 1 (ALSC) + Group 2 (backoff).
//!
//! These tests run in-process (no server, no containers) using #[tokio::test].
//! They cover:
//!   - `ConnectivityManager::backoff_duration`: sequence, overflow safety, heartbeat off-by-one
//!   - `ConnectivityManager` state-machine transitions
//!   - `AgentStateCache::derive_routes_map` / `derive_dns_map`: routing and DNS derivation logic
//!   - `AgentStore::open` / `save` / `load` / `load_routes` / `load_dns_records`: SlateDB round-trips

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod helpers {
    use chrono::Utc;
    use pkg_types::{
        endpoint::{Endpoint, EndpointAddress},
        service::{Service, ServicePort, ServiceSpec, ServiceType},
    };
    use std::collections::HashMap;

    /// Build a Service fixture with a single TCP port.
    pub fn make_service(
        id: &str,
        name: &str,
        ns: &str,
        cluster_ip: &str,
        port: u16,
        target_port: u16,
    ) -> Service {
        Service {
            id: id.to_string(),
            name: name.to_string(),
            namespace: ns.to_string(),
            cluster_ip: Some(cluster_ip.to_string()),
            vpc: Some("default".to_string()),
            spec: ServiceSpec {
                selector: HashMap::new(),
                ports: vec![ServicePort {
                    name: "http".to_string(),
                    port,
                    target_port,
                    node_port: None,
                }],
                service_type: ServiceType::ClusterIP,
            },
            created_at: Utc::now(),
        }
    }

    /// Build an Endpoint fixture with a single pod IP address.
    pub fn make_endpoint(
        id: &str,
        service_id: &str,
        service_name: &str,
        ns: &str,
        pod_ip: &str,
    ) -> Endpoint {
        Endpoint {
            id: id.to_string(),
            service_id: service_id.to_string(),
            service_name: service_name.to_string(),
            namespace: ns.to_string(),
            addresses: vec![EndpointAddress {
                ip: pod_ip.to_string(),
                node_name: None,
                pod_id: None,
            }],
            ports: vec![],
            created_at: Utc::now(),
        }
    }

    /// Create and return a unique temp directory path (does NOT create the dir).
    pub fn temp_dir(label: &str) -> String {
        let ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        format!("/tmp/k3rs-test-{}-{}", label, ns)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Group 2 — Backoff
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod backoff_tests {
    use crate::connectivity::ConnectivityManager;
    use std::time::Duration;

    /// Verify the full 1s → 2s → 4s → 8s → 16s → 30s sequence.
    #[test]
    fn backoff_sequence_is_correct() {
        let cases = [
            (0, 1u64),
            (1, 2),
            (2, 4),
            (3, 8),
            (4, 16),
            (5, 30), // 32 → capped to 30
            (6, 30),
            (10, 30),
        ];
        for (attempt, expected_secs) in cases {
            assert_eq!(
                ConnectivityManager::backoff_duration(attempt),
                Duration::from_secs(expected_secs),
                "attempt={} must give {}s",
                attempt,
                expected_secs
            );
        }
    }

    /// Regression: `1u64 << attempt` panicked when attempt >= 64.
    /// Fixed by capping shift index at 30 (`attempt.min(30)`).
    #[test]
    fn backoff_does_not_panic_on_large_attempt() {
        let _ = ConnectivityManager::backoff_duration(63);
        let _ = ConnectivityManager::backoff_duration(64); // was panic before fix
        let _ = ConnectivityManager::backoff_duration(128);
        let _ = ConnectivityManager::backoff_duration(u32::MAX);
    }

    /// Verify the heartbeat off-by-one fix: `fail_count.saturating_sub(1)`.
    ///
    /// In the heartbeat loop, `fail_count` is incremented *after* a failure and
    /// is therefore 1-based at the top of the next loop iteration. The fix
    /// subtracts 1 to get a 0-based index, so the first retry fires after 1s
    /// rather than 2s.
    #[test]
    fn heartbeat_backoff_is_1s_on_first_retry() {
        // fail_count=1 after first failure → saturating_sub(1) = 0 → 1s
        let first = ConnectivityManager::backoff_duration(1u32.saturating_sub(1));
        assert_eq!(
            first,
            Duration::from_secs(1),
            "first heartbeat retry must be 1s"
        );

        // fail_count=2 → index 1 → 2s
        let second = ConnectivityManager::backoff_duration(2u32.saturating_sub(1));
        assert_eq!(second, Duration::from_secs(2));

        // fail_count=3 → index 2 → 4s
        let third = ConnectivityManager::backoff_duration(3u32.saturating_sub(1));
        assert_eq!(third, Duration::from_secs(4));
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ConnectivityManager state machine
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod connectivity_tests {
    use crate::connectivity::{ConnectivityManager, ConnectivityState};

    #[test]
    fn initial_state_is_connecting() {
        let cm = ConnectivityManager::new();
        assert!(matches!(cm.state(), ConnectivityState::Connecting));
        assert!(!cm.is_connected());
    }

    #[test]
    fn connecting_to_connected() {
        let cm = ConnectivityManager::new();
        cm.set_connected();
        assert!(cm.is_connected());
        assert!(matches!(cm.state(), ConnectivityState::Connected));
    }

    #[test]
    fn connected_to_reconnecting() {
        let cm = ConnectivityManager::new();
        cm.set_connected();
        cm.set_reconnecting(1);
        assert!(!cm.is_connected());
        assert!(matches!(
            cm.state(),
            ConnectivityState::Reconnecting { attempt: 1 }
        ));
    }

    #[test]
    fn connected_to_offline() {
        let cm = ConnectivityManager::new();
        cm.set_connected();
        cm.set_offline();
        assert!(!cm.is_connected());
        assert!(matches!(cm.state(), ConnectivityState::Offline));
    }

    #[test]
    fn offline_to_connected() {
        let cm = ConnectivityManager::new();
        cm.set_offline();
        cm.set_connected();
        assert!(cm.is_connected());
    }

    #[test]
    fn reconnecting_to_connected() {
        let cm = ConnectivityManager::new();
        cm.set_reconnecting(5);
        cm.set_connected();
        assert!(cm.is_connected());
    }

    #[test]
    fn reconnecting_attempt_counter_increments() {
        let cm = ConnectivityManager::new();
        for i in 1..=5 {
            cm.set_reconnecting(i);
            assert!(matches!(
                cm.state(),
                ConnectivityState::Reconnecting { attempt } if attempt == i
            ));
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Group 1 — AgentStateCache derivation (pure, no I/O)
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod cache_derivation_tests {
    use super::helpers::{make_endpoint, make_service};
    use crate::cache::AgentStateCache;
    use pkg_types::endpoint::EndpointAddress;

    // ---- DNS map ----

    #[test]
    fn derive_dns_map_fqdn_format() {
        let mut c = AgentStateCache::new("node".to_string());
        c.services = vec![
            make_service("svc-1", "nginx", "default", "10.0.0.1", 80, 8080),
            make_service("svc-2", "redis", "cache", "10.0.0.2", 6379, 6379),
        ];

        let dns = c.derive_dns_map();

        assert_eq!(
            dns.get("nginx.default.svc.cluster.local")
                .map(String::as_str),
            Some("10.0.0.1"),
            "nginx FQDN must map to ClusterIP"
        );
        assert_eq!(
            dns.get("redis.cache.svc.cluster.local").map(String::as_str),
            Some("10.0.0.2"),
            "cross-namespace FQDN must be correct"
        );
        assert_eq!(dns.len(), 2);
    }

    #[test]
    fn derive_dns_map_skips_headless_services() {
        let mut c = AgentStateCache::new("node".to_string());
        let mut svc = make_service("svc-1", "headless", "default", "dummy", 80, 80);
        svc.cluster_ip = None;
        c.services = vec![svc];

        assert!(c.derive_dns_map().is_empty(), "headless → no DNS record");
    }

    #[test]
    fn derive_dns_map_empty_cache_returns_empty_map() {
        let c = AgentStateCache::new("node".to_string());
        assert!(c.derive_dns_map().is_empty());
    }

    // ---- Routes map ----

    #[test]
    fn derive_routes_map_basic_single_backend() {
        let mut c = AgentStateCache::new("node".to_string());
        c.services = vec![make_service(
            "svc-1", "nginx", "default", "10.0.0.1", 80, 8080,
        )];
        c.endpoints = vec![make_endpoint(
            "ep-1",
            "svc-1",
            "nginx",
            "default",
            "192.168.1.10",
        )];

        let routes = c.derive_routes_map();
        let backends = routes
            .get("10.0.0.1:80")
            .expect("route 10.0.0.1:80 must exist");
        assert_eq!(backends, &vec!["192.168.1.10:8080".to_string()]);
    }

    #[test]
    fn derive_routes_map_multiple_backends() {
        let mut c = AgentStateCache::new("node".to_string());
        c.services = vec![make_service(
            "svc-1", "nginx", "default", "10.0.0.1", 80, 8080,
        )];
        let mut ep = make_endpoint("ep-1", "svc-1", "nginx", "default", "192.168.1.10");
        ep.addresses.push(EndpointAddress {
            ip: "192.168.1.11".to_string(),
            node_name: None,
            pod_id: None,
        });
        c.endpoints = vec![ep];

        let routes = c.derive_routes_map();
        let backends = routes.get("10.0.0.1:80").unwrap();
        assert_eq!(backends.len(), 2, "two pod IPs → two backends");
        assert!(backends.contains(&"192.168.1.10:8080".to_string()));
        assert!(backends.contains(&"192.168.1.11:8080".to_string()));
    }

    #[test]
    fn derive_routes_map_skips_service_without_cluster_ip() {
        let mut c = AgentStateCache::new("node".to_string());
        let mut svc = make_service("svc-1", "nginx", "default", "dummy", 80, 8080);
        svc.cluster_ip = None;
        c.services = vec![svc];
        c.endpoints = vec![make_endpoint(
            "ep-1",
            "svc-1",
            "nginx",
            "default",
            "192.168.1.10",
        )];

        assert!(
            c.derive_routes_map().is_empty(),
            "service without ClusterIP must not produce routes"
        );
    }

    #[test]
    fn derive_routes_map_no_backends_when_no_matching_endpoints() {
        let mut c = AgentStateCache::new("node".to_string());
        c.services = vec![make_service(
            "svc-1", "nginx", "default", "10.0.0.1", 80, 8080,
        )];
        // No endpoints → no backends → route key omitted entirely
        assert!(
            c.derive_routes_map().get("10.0.0.1:80").is_none(),
            "no endpoints → no route entry"
        );
    }

    #[test]
    fn derive_routes_map_ignores_endpoints_from_different_namespace() {
        let mut c = AgentStateCache::new("node".to_string());
        c.services = vec![make_service(
            "svc-1", "nginx", "default", "10.0.0.1", 80, 8080,
        )];
        // Endpoint is in "other-ns" — different namespace → should not match
        c.endpoints = vec![make_endpoint(
            "ep-1",
            "svc-1",
            "nginx",
            "other-ns",
            "192.168.1.10",
        )];

        assert!(
            c.derive_routes_map().get("10.0.0.1:80").is_none(),
            "endpoint in different namespace must not match"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Group 1 — AgentStore (SlateDB) round-trips
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod agent_store_tests {
    use super::helpers::{make_endpoint, make_service, temp_dir};
    use crate::{cache::AgentStateCache, store::AgentStore};
    use chrono::Utc;

    /// Scenario 4 (fresh start): fresh store must report no cached state.
    #[tokio::test]
    async fn fresh_store_load_returns_none() {
        let dir = temp_dir("fresh");
        let store = AgentStore::open(&dir).await.unwrap();
        assert!(
            store.load().await.unwrap().is_none(),
            "fresh store must return None (no /agent/meta key)"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Identity fields (node_name, node_id, agent_api_port, server_seq) survive
    /// a save → reload cycle.
    #[tokio::test]
    async fn roundtrip_identity_fields() {
        let dir = temp_dir("identity");
        let store = AgentStore::open(&dir).await.unwrap();

        let mut cache = AgentStateCache::new("test-node".to_string());
        cache.node_id = Some("node-abc".to_string());
        cache.agent_api_port = Some(9443);
        cache.server_seq = 42;
        cache.last_synced_at = Utc::now();

        store.save(&cache).await.unwrap();
        let loaded = store.load().await.unwrap().expect("must load saved cache");

        assert_eq!(loaded.node_name, "test-node");
        assert_eq!(loaded.node_id.as_deref(), Some("node-abc"));
        assert_eq!(loaded.agent_api_port, Some(9443));
        assert_eq!(loaded.server_seq, 42);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Services and endpoints survive a save → reload cycle with correct counts
    /// and field values.
    #[tokio::test]
    async fn roundtrip_services_and_endpoints() {
        let dir = temp_dir("collections");
        let store = AgentStore::open(&dir).await.unwrap();

        let mut cache = AgentStateCache::new("node".to_string());
        cache.services = vec![
            make_service("svc-1", "nginx", "default", "10.0.0.1", 80, 8080),
            make_service("svc-2", "redis", "cache", "10.0.0.2", 6379, 6379),
        ];
        cache.endpoints = vec![
            make_endpoint("ep-1", "svc-1", "nginx", "default", "192.168.1.10"),
            make_endpoint("ep-2", "svc-1", "nginx", "default", "192.168.1.11"),
        ];

        store.save(&cache).await.unwrap();
        let loaded = store.load().await.unwrap().unwrap();

        assert_eq!(loaded.services.len(), 2, "service count must be preserved");
        assert_eq!(
            loaded.endpoints.len(),
            2,
            "endpoint count must be preserved"
        );

        let nginx = loaded.services.iter().find(|s| s.name == "nginx").unwrap();
        assert_eq!(nginx.cluster_ip.as_deref(), Some("10.0.0.1"));
        assert_eq!(nginx.namespace, "default");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Scenario 2 (stale cache): derived views (`/agent/routes`, `/agent/dns-records`)
    /// are stored atomically and readable via the fast-path helpers.
    /// This simulates the bootstrap path where only 2 keys need to be read
    /// instead of scanning all collections.
    #[tokio::test]
    async fn derived_views_are_stored_and_fast_loadable() {
        let dir = temp_dir("derived");
        let store = AgentStore::open(&dir).await.unwrap();

        let mut cache = AgentStateCache::new("node".to_string());
        cache.services = vec![make_service(
            "svc-1",
            "nginx",
            "default",
            "10.0.0.50",
            80,
            8080,
        )];
        cache.endpoints = vec![make_endpoint(
            "ep-1",
            "svc-1",
            "nginx",
            "default",
            "192.168.1.5",
        )];

        store.save(&cache).await.unwrap();

        // Fast path: load_routes / load_dns_records (Scenario 2 bootstrap)
        let routes = store
            .load_routes()
            .await
            .unwrap()
            .expect("routes must be stored");
        assert!(routes.contains_key("10.0.0.50:80"), "route key must exist");
        assert_eq!(routes["10.0.0.50:80"], vec!["192.168.1.5:8080"]);

        let dns = store
            .load_dns_records()
            .await
            .unwrap()
            .expect("dns must be stored");
        assert_eq!(
            dns.get("nginx.default.svc.cluster.local")
                .map(String::as_str),
            Some("10.0.0.50")
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Scenario 5 (stale cache / server-wins): second `save()` overwrites all
    /// keys from the first — no merging, server state always wins.
    #[tokio::test]
    async fn second_save_overwrites_first_server_wins() {
        let dir = temp_dir("overwrite");
        let store = AgentStore::open(&dir).await.unwrap();

        // First sync: service A only
        let mut cache = AgentStateCache::new("node".to_string());
        cache.server_seq = 1;
        cache.services = vec![make_service(
            "svc-a", "svc-a", "default", "10.0.0.1", 80, 8080,
        )];
        store.save(&cache).await.unwrap();

        // Second sync: service A replaced by service B (simulates server-wins re-sync)
        cache.server_seq = 99;
        cache.services = vec![make_service(
            "svc-b",
            "svc-b",
            "default",
            "10.0.0.99",
            80,
            8080,
        )];
        store.save(&cache).await.unwrap();

        let loaded = store.load().await.unwrap().unwrap();
        assert_eq!(loaded.server_seq, 99, "second save must win");
        assert_eq!(
            loaded.services.len(),
            1,
            "stale svc-a must be gone — full-array overwrite replaces entire collection"
        );
        assert_eq!(
            loaded.services[0].name, "svc-b",
            "svc-b must be present after re-sync"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Opening the same store path twice returns independent handles to the same data.
    #[tokio::test]
    async fn reopen_reads_persisted_data() {
        let dir = temp_dir("reopen");

        {
            let store = AgentStore::open(&dir).await.unwrap();
            let mut cache = AgentStateCache::new("persistent-node".to_string());
            cache.server_seq = 77;
            store.save(&cache).await.unwrap();
            store.close().await.unwrap();
        }

        // Simulate process restart: open a new store handle to the same path
        {
            let store2 = AgentStore::open(&dir).await.unwrap();
            let loaded = store2
                .load()
                .await
                .unwrap()
                .expect("data must survive close+reopen");
            assert_eq!(loaded.node_name, "persistent-node");
            assert_eq!(
                loaded.server_seq, 77,
                "server_seq must persist across restart"
            );
        }

        std::fs::remove_dir_all(&dir).ok();
    }
}
