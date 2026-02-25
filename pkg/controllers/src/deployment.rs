use chrono::Utc;
use pkg_state::client::StateStore;
use pkg_types::deployment::{Deployment, DeploymentStrategy};
use pkg_types::replicaset::{ReplicaSet, ReplicaSetSpec, ReplicaSetStatus};
use std::time::Duration;
use tracing::{info, warn};
use uuid::Uuid;

/// Controller that reconciles Deployments into ReplicaSets.
/// Handles rolling updates and recreate strategies.
pub struct DeploymentController {
    store: StateStore,
    check_interval: Duration,
}

impl DeploymentController {
    pub fn new(store: StateStore) -> Self {
        Self {
            store,
            check_interval: Duration::from_secs(10),
        }
    }

    pub fn start(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            info!(
                "DeploymentController started (interval={}s)",
                self.check_interval.as_secs()
            );
            let mut interval = tokio::time::interval(self.check_interval);
            loop {
                interval.tick().await;
                if let Err(e) = self.reconcile().await {
                    warn!("DeploymentController reconcile error: {}", e);
                }
            }
        })
    }

    async fn reconcile(&self) -> anyhow::Result<()> {
        // Get all namespaces
        let ns_entries = self.store.list_prefix("/registry/namespaces/").await?;
        for (ns_key, _) in ns_entries {
            let ns = ns_key
                .strip_prefix("/registry/namespaces/")
                .unwrap_or_default()
                .to_string();
            if ns.is_empty() {
                continue;
            }
            self.reconcile_namespace(&ns).await?;
        }
        Ok(())
    }

    async fn reconcile_namespace(&self, ns: &str) -> anyhow::Result<()> {
        let deploy_prefix = format!("/registry/deployments/{}/", ns);
        let deploy_entries = self.store.list_prefix(&deploy_prefix).await?;

        for (key, value) in deploy_entries {
            let mut deploy: Deployment = match serde_json::from_slice(&value) {
                Ok(d) => d,
                Err(_) => continue,
            };

            // Compute a simple template hash from the deployment spec
            let template_hash = compute_template_hash(&deploy);

            // Find existing ReplicaSets owned by this deployment
            let rs_prefix = format!("/registry/replicasets/{}/", ns);
            let rs_entries = self.store.list_prefix(&rs_prefix).await?;
            let owned_rs: Vec<(String, ReplicaSet)> = rs_entries
                .into_iter()
                .filter_map(|(k, v)| {
                    let rs: ReplicaSet = serde_json::from_slice(&v).ok()?;
                    if rs.owner_ref.as_deref() == Some(&deploy.id) {
                        Some((k, rs))
                    } else {
                        None
                    }
                })
                .collect();

            // Find the RS matching the current template
            let current_rs = owned_rs
                .iter()
                .find(|(_, rs)| rs.template_hash == template_hash);

            match &deploy.spec.strategy {
                DeploymentStrategy::RollingUpdate {
                    max_surge,
                    max_unavailable: _,
                } => {
                    let max_surge = *max_surge;

                    if current_rs.is_none() {
                        // Create new ReplicaSet
                        let rs = self
                            .create_replicaset(
                                ns,
                                &deploy,
                                &template_hash,
                                deploy.spec.replicas.min(max_surge),
                            )
                            .await?;
                        info!(
                            "Deployment {}: created new RS {} (hash={})",
                            deploy.name, rs.name, template_hash
                        );

                        // Scale down old ReplicaSets
                        for (old_key, mut old_rs) in owned_rs {
                            if old_rs.template_hash != template_hash && old_rs.spec.replicas > 0 {
                                old_rs.spec.replicas =
                                    old_rs.spec.replicas.saturating_sub(max_surge.max(1));
                                let data = serde_json::to_vec(&old_rs)?;
                                self.store.put(&old_key, &data).await?;
                                info!(
                                    "Deployment {}: scaling down old RS {} to {}",
                                    deploy.name, old_rs.name, old_rs.spec.replicas
                                );
                            }
                        }
                    } else {
                        // Current RS exists — make sure it has the right replica count
                        let (rs_key, rs) = current_rs.unwrap();
                        if rs.spec.replicas != deploy.spec.replicas {
                            let mut rs = rs.clone();
                            rs.spec.replicas = deploy.spec.replicas;
                            let data = serde_json::to_vec(&rs)?;
                            self.store.put(rs_key, &data).await?;
                            info!(
                                "Deployment {}: scaled RS {} to {}",
                                deploy.name, rs.name, deploy.spec.replicas
                            );
                        }

                        // Scale down any old RS to 0
                        for (old_key, mut old_rs) in owned_rs.into_iter() {
                            if old_rs.template_hash != template_hash && old_rs.spec.replicas > 0 {
                                old_rs.spec.replicas = 0;
                                let data = serde_json::to_vec(&old_rs)?;
                                self.store.put(&old_key, &data).await?;
                            }
                        }
                    }
                }
                DeploymentStrategy::Recreate => {
                    if current_rs.is_none() {
                        // Scale all old RS to 0 first
                        for (old_key, mut old_rs) in &mut owned_rs.into_iter() {
                            if old_rs.spec.replicas > 0 {
                                old_rs.spec.replicas = 0;
                                let data = serde_json::to_vec(&old_rs)?;
                                self.store.put(&old_key, &data).await?;
                            }
                        }
                        // Then create new RS at full scale
                        self.create_replicaset(ns, &deploy, &template_hash, deploy.spec.replicas)
                            .await?;
                        info!("Deployment {}: recreated with new RS", deploy.name);
                    } else {
                        let (rs_key, rs) = current_rs.unwrap();
                        if rs.spec.replicas != deploy.spec.replicas {
                            let mut rs = rs.clone();
                            rs.spec.replicas = deploy.spec.replicas;
                            let data = serde_json::to_vec(&rs)?;
                            self.store.put(rs_key, &data).await?;
                        }
                    }
                }
            }

            // Update deployment status from owned ReplicaSets
            let rs_prefix = format!("/registry/replicasets/{}/", ns);
            let rs_entries = self.store.list_prefix(&rs_prefix).await?;
            let mut ready = 0u32;
            let mut available = 0u32;
            let mut updated = 0u32;
            for (_, v) in rs_entries {
                if let Ok(rs) = serde_json::from_slice::<ReplicaSet>(&v) {
                    if rs.owner_ref.as_deref() == Some(&deploy.id) {
                        ready += rs.status.ready_replicas;
                        available += rs.status.available_replicas;
                        if rs.template_hash == template_hash {
                            updated += rs.status.ready_replicas;
                        }
                    }
                }
            }

            deploy.status.ready_replicas = ready;
            deploy.status.available_replicas = available;
            deploy.status.updated_replicas = updated;
            deploy.observed_generation = deploy.generation;
            let data = serde_json::to_vec(&deploy)?;
            self.store.put(&key, &data).await?;
        }
        Ok(())
    }

    async fn create_replicaset(
        &self,
        ns: &str,
        deploy: &Deployment,
        template_hash: &str,
        replicas: u32,
    ) -> anyhow::Result<ReplicaSet> {
        let rs_id = Uuid::new_v4().to_string();
        let rs = ReplicaSet {
            id: rs_id.clone(),
            name: format!(
                "{}-{}",
                deploy.name,
                &template_hash[..8.min(template_hash.len())]
            ),
            namespace: ns.to_string(),
            spec: ReplicaSetSpec {
                replicas,
                selector: deploy.spec.selector.clone(),
                template: deploy.spec.template.clone(),
            },
            status: ReplicaSetStatus::default(),
            owner_ref: Some(deploy.id.clone()),
            template_hash: template_hash.to_string(),
            created_at: Utc::now(),
        };
        let key = format!("/registry/replicasets/{}/{}", ns, rs_id);
        let data = serde_json::to_vec(&rs)?;
        self.store.put(&key, &data).await?;
        Ok(rs)
    }
}

/// Compute a simple hash of the deployment template for change detection.
fn compute_template_hash(deploy: &Deployment) -> String {
    let json = serde_json::to_string(&deploy.spec.template).unwrap_or_default();
    // Simple hash: use first 16 chars of the hex-encoded bytes checksum
    let mut hash: u64 = 0;
    for byte in json.bytes() {
        hash = hash.wrapping_mul(31).wrapping_add(byte as u64);
    }
    format!("{:016x}", hash)
}
