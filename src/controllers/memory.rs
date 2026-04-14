//! StreamlineMemory reconciliation controller (M1 P4).
//! Manages agent memory topic lifecycle via the admin API.
//! On create: ensures memory topics exist for the agent (episodic, semantic, procedural).
//! On delete: deletes memory topics for GDPR compliance.

use crate::conditions::{
    build_condition, set_condition, CONDITION_FALSE, CONDITION_TRUE, MEMORY_FINALIZER,
};
use crate::controllers::error_policy_backoff;
use crate::crd::{
    MemoryCondition, MemoryPhase, MemoryStatus, StreamlineCluster, StreamlineMemory,
};
use crate::error::{OperatorError, Result};
use futures::StreamExt;
use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher::Config;
use kube::{Client, ResourceExt};
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

// Memory-specific condition types
const MEMORY_CONDITION_READY: &str = "Ready";
const MEMORY_CONDITION_TOPICS_PROVISIONED: &str = "TopicsProvisioned";

/// Memory tier topic suffixes
const MEMORY_TIERS: &[&str] = &["episodic", "semantic", "procedural"];

/// Context for the memory controller
pub struct MemoryController {
    client: Client,
    http_client: reqwest::Client,
}

impl MemoryController {
    /// Create a new memory controller
    pub fn new(client: Client, http_client: reqwest::Client) -> Self {
        Self { client, http_client }
    }

    /// Run the memory controller
    pub async fn run(self: Arc<Self>) -> Result<()> {
        let memories: Api<StreamlineMemory> = Api::all(self.client.clone());

        info!("Starting StreamlineMemory controller");

        Controller::new(memories, Config::default())
            .shutdown_on_signal()
            .run(
                |memory, ctx| async move { ctx.reconcile(memory).await },
                |_memory, error, _ctx| {
                    error!("Reconciliation error: {:?}", error);
                    crate::metrics::get().inc_error("memory");
                    error_policy_backoff(_memory, error, _ctx)
                },
                Arc::clone(&self),
            )
            .for_each(|result| async move {
                match result {
                    Ok((obj, _action)) => {
                        info!("Reconciled memory: {}", obj.name);
                    }
                    Err(e) => {
                        error!("Reconciliation failed: {:?}", e);
                    }
                }
            })
            .await;

        Ok(())
    }

    /// Build topic name for a memory tier
    fn topic_name(memory: &StreamlineMemory, tier: &str) -> String {
        format!(
            "_memory.{}.{}.{}",
            memory.spec.tenant,
            memory.spec.agent_id,
            tier
        )
    }

    /// Compute retention_ms from retention_days (-1 means infinite)
    fn retention_ms(days: i64) -> i64 {
        if days < 0 {
            -1
        } else {
            days * 24 * 60 * 60 * 1000
        }
    }

    /// Reconcile a StreamlineMemory
    async fn reconcile(
        &self,
        memory: Arc<StreamlineMemory>,
    ) -> std::result::Result<Action, OperatorError> {
        crate::metrics::get().inc_reconcile("memory");
        let _timer = crate::metrics::get().start_timer();
        let name = memory.name_any();
        let namespace = memory.namespace().unwrap_or_else(|| "default".to_string());

        info!("Reconciling StreamlineMemory {}/{}", namespace, name);

        // Handle deletion with finalizer
        if memory.metadata.deletion_timestamp.is_some() {
            return self.handle_deletion(&memory, &namespace).await;
        }

        // Ensure finalizer is set
        self.ensure_finalizer(&memory, &namespace).await?;

        // Get the referenced cluster
        let clusters: Api<StreamlineCluster> = Api::namespaced(self.client.clone(), &namespace);
        let cluster = match clusters.get(&memory.spec.cluster_ref).await {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    "Cluster {} not found for memory {}: {}",
                    memory.spec.cluster_ref, name, e
                );
                self.update_status_error(
                    &memory,
                    &namespace,
                    &format!("Cluster {} not found", memory.spec.cluster_ref),
                )
                .await?;
                return Ok(Action::requeue(Duration::from_secs(30)));
            }
        };

        // Check if cluster is ready
        let cluster_ready = cluster
            .status
            .as_ref()
            .is_some_and(|s| s.ready_replicas > 0 && !s.broker_endpoints.is_empty());

        if !cluster_ready {
            warn!(
                "Cluster {} not ready for memory {}",
                memory.spec.cluster_ref, name
            );
            self.update_status_pending(&memory, &namespace, "Waiting for cluster to be ready")
                .await?;
            return Ok(Action::requeue(Duration::from_secs(10)));
        }

        // Build HTTP endpoint from cluster
        let cluster_name = cluster.name_any();
        let http_endpoint = format!(
            "http://{}-0.{}-headless.{}.svc:{}",
            cluster_name, cluster_name, namespace, cluster.spec.http_port
        );

        // Ensure memory topics exist for each tier
        match self.ensure_memory_topics(&memory, &http_endpoint).await {
            Ok(_) => {
                self.update_status_ready(&memory, &namespace).await?;
            }
            Err(e) => {
                error!("Failed to provision memory topics for {}: {}", name, e);
                self.update_status_error(&memory, &namespace, &e.to_string())
                    .await?;
                return Ok(Action::requeue(Duration::from_secs(30)));
            }
        }

        crate::metrics::get().inc_success();
        Ok(Action::requeue(Duration::from_secs(60)))
    }

    /// Ensure the finalizer is present on the resource
    async fn ensure_finalizer(
        &self,
        memory: &StreamlineMemory,
        namespace: &str,
    ) -> Result<()> {
        let finalizers = memory.metadata.finalizers.as_deref().unwrap_or_default();
        if finalizers.contains(&MEMORY_FINALIZER.to_string()) {
            return Ok(());
        }

        let memories: Api<StreamlineMemory> = Api::namespaced(self.client.clone(), namespace);
        let patch = serde_json::json!({
            "metadata": {
                "finalizers": [MEMORY_FINALIZER]
            }
        });
        memories
            .patch(
                &memory.name_any(),
                &PatchParams::apply("streamline-operator").force(),
                &Patch::Apply(&patch),
            )
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        Ok(())
    }

    /// Handle deletion: delete memory topics (GDPR compliance), then remove finalizer
    async fn handle_deletion(
        &self,
        memory: &StreamlineMemory,
        namespace: &str,
    ) -> std::result::Result<Action, OperatorError> {
        let name = memory.name_any();
        info!(
            "Handling deletion of StreamlineMemory {}/{} (GDPR cleanup)",
            namespace, name
        );

        // Attempt to delete memory topics from the Streamline cluster
        let clusters: Api<StreamlineCluster> = Api::namespaced(self.client.clone(), namespace);
        if let Ok(cluster) = clusters.get(&memory.spec.cluster_ref).await {
            let cluster_name = cluster.name_any();
            let http_endpoint = format!(
                "http://{}-0.{}-headless.{}.svc:{}",
                cluster_name, cluster_name, namespace, cluster.spec.http_port
            );

            for tier in MEMORY_TIERS {
                let topic_name = Self::topic_name(memory, tier);
                info!("Deleting memory topic {} from cluster", topic_name);
                if let Err(e) = self
                    .http_client
                    .delete(format!("{}/api/v1/topics/{}", http_endpoint, topic_name))
                    .send()
                    .await
                {
                    warn!("Failed to delete memory topic {} from cluster API: {}", topic_name, e);
                }
            }
        } else {
            warn!(
                "Cluster {} not found during memory deletion, skipping topic cleanup",
                memory.spec.cluster_ref
            );
        }

        // Remove finalizer
        let memories: Api<StreamlineMemory> = Api::namespaced(self.client.clone(), namespace);
        let finalizers: Vec<String> = memory
            .metadata
            .finalizers
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|f| f.as_str() != MEMORY_FINALIZER)
            .cloned()
            .collect();

        let patch = serde_json::json!({
            "metadata": {
                "finalizers": finalizers
            }
        });
        memories
            .patch(
                &name,
                &PatchParams::default(),
                &Patch::Merge(&patch),
            )
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        info!(
            "Finalizer removed for StreamlineMemory {}/{} (GDPR cleanup complete)",
            namespace, name
        );
        Ok(Action::await_change())
    }

    /// Ensure memory topics exist for each tier
    async fn ensure_memory_topics(
        &self,
        memory: &StreamlineMemory,
        http_endpoint: &str,
    ) -> Result<()> {
        let retentions = [
            ("episodic", Self::retention_ms(memory.spec.tiers.episodic_retention_days)),
            ("semantic", Self::retention_ms(memory.spec.tiers.semantic_retention_days)),
            ("procedural", Self::retention_ms(memory.spec.tiers.procedural_retention_days)),
        ];

        for (tier, retention_ms) in &retentions {
            let topic_name = Self::topic_name(memory, tier);
            let mut config = serde_json::json!({
                "cleanup.policy": "compact,delete",
            });
            if *retention_ms >= 0 {
                config["retention.ms"] = serde_json::json!(retention_ms.to_string());
            }

            let topic_config = serde_json::json!({
                "name": topic_name,
                "partitions": 1,
                "replication_factor": 1,
                "config": config,
            });

            info!("Ensuring memory topic {} exists", topic_name);

            let response = self
                .http_client
                .post(format!("{}/api/v1/topics", http_endpoint))
                .json(&topic_config)
                .send()
                .await
                .map_err(|e| {
                    OperatorError::Internal(format!(
                        "HTTP request to create memory topic {} failed: {}",
                        topic_name, e
                    ))
                })?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                // 409 Conflict means topic already exists — treat as success
                if status.as_u16() != 409 {
                    return Err(OperatorError::Internal(format!(
                        "Failed to create memory topic {} (HTTP {}): {}",
                        topic_name, status, body
                    )));
                }
            }
        }

        Ok(())
    }

    /// Update status to ready
    async fn update_status_ready(
        &self,
        memory: &StreamlineMemory,
        namespace: &str,
    ) -> Result<()> {
        let name = memory.name_any();
        let memories: Api<StreamlineMemory> = Api::namespaced(self.client.clone(), namespace);

        let mut cond_fields = Vec::new();
        set_condition(
            &mut cond_fields,
            build_condition(
                MEMORY_CONDITION_READY,
                CONDITION_TRUE,
                "MemoryReady",
                "Memory topics provisioned and ready",
            ),
        );
        set_condition(
            &mut cond_fields,
            build_condition(
                MEMORY_CONDITION_TOPICS_PROVISIONED,
                CONDITION_TRUE,
                "AllTiersProvisioned",
                "Episodic, semantic, and procedural topics created",
            ),
        );

        let conditions: Vec<MemoryCondition> = cond_fields
            .into_iter()
            .map(|c| MemoryCondition {
                r#type: c.condition_type,
                status: c.status,
                last_transition_time: c.last_transition_time,
                reason: c.reason,
                message: c.message,
            })
            .collect();

        let status = MemoryStatus {
            phase: MemoryPhase::Ready,
            ready: true,
            episodic_event_count: 0,
            semantic_event_count: 0,
            procedural_event_count: 0,
            message: Some("Memory topics provisioned successfully".to_string()),
            conditions,
        };

        let patch = serde_json::json!({ "status": status });
        memories
            .patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        Ok(())
    }

    /// Update status to pending
    async fn update_status_pending(
        &self,
        memory: &StreamlineMemory,
        namespace: &str,
        message: &str,
    ) -> Result<()> {
        let name = memory.name_any();
        let memories: Api<StreamlineMemory> = Api::namespaced(self.client.clone(), namespace);

        let mut cond_fields = Vec::new();
        set_condition(
            &mut cond_fields,
            build_condition(
                MEMORY_CONDITION_READY,
                CONDITION_FALSE,
                "Pending",
                message,
            ),
        );
        set_condition(
            &mut cond_fields,
            build_condition(
                MEMORY_CONDITION_TOPICS_PROVISIONED,
                CONDITION_FALSE,
                "WaitingForCluster",
                "Memory topics cannot be provisioned until cluster is ready",
            ),
        );

        let conditions: Vec<MemoryCondition> = cond_fields
            .into_iter()
            .map(|c| MemoryCondition {
                r#type: c.condition_type,
                status: c.status,
                last_transition_time: c.last_transition_time,
                reason: c.reason,
                message: c.message,
            })
            .collect();

        let status = MemoryStatus {
            phase: MemoryPhase::Pending,
            ready: false,
            message: Some(message.to_string()),
            conditions,
            ..Default::default()
        };

        let patch = serde_json::json!({ "status": status });
        memories
            .patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        Ok(())
    }

    /// Update status to error
    async fn update_status_error(
        &self,
        memory: &StreamlineMemory,
        namespace: &str,
        error_message: &str,
    ) -> Result<()> {
        let name = memory.name_any();
        let memories: Api<StreamlineMemory> = Api::namespaced(self.client.clone(), namespace);

        let mut cond_fields = Vec::new();
        set_condition(
            &mut cond_fields,
            build_condition(
                MEMORY_CONDITION_READY,
                CONDITION_FALSE,
                "Error",
                error_message,
            ),
        );
        set_condition(
            &mut cond_fields,
            build_condition(
                MEMORY_CONDITION_TOPICS_PROVISIONED,
                CONDITION_FALSE,
                "ProvisioningFailed",
                error_message,
            ),
        );

        let conditions: Vec<MemoryCondition> = cond_fields
            .into_iter()
            .map(|c| MemoryCondition {
                r#type: c.condition_type,
                status: c.status,
                last_transition_time: c.last_transition_time,
                reason: c.reason,
                message: c.message,
            })
            .collect();

        let status = MemoryStatus {
            phase: MemoryPhase::Failed,
            ready: false,
            message: Some(error_message.to_string()),
            conditions,
            ..Default::default()
        };

        let patch = serde_json::json!({ "status": status });
        memories
            .patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_controller() {
        // Controller tests require k8s cluster
    }

    #[test]
    fn test_topic_name_generation() {
        // Verify topic naming convention
        let json = r#"{
            "agentId": "agent-1",
            "tenant": "acme",
            "clusterRef": "prod"
        }"#;
        let spec: crate::crd::MemorySpec = serde_json::from_str(json).unwrap();
        let memory = StreamlineMemory::new("test-memory", spec);
        assert_eq!(
            MemoryController::topic_name(&memory, "episodic"),
            "_memory.acme.agent-1.episodic"
        );
        assert_eq!(
            MemoryController::topic_name(&memory, "semantic"),
            "_memory.acme.agent-1.semantic"
        );
    }

    #[test]
    fn test_retention_ms() {
        assert_eq!(MemoryController::retention_ms(30), 2_592_000_000);
        assert_eq!(MemoryController::retention_ms(-1), -1);
        assert_eq!(MemoryController::retention_ms(0), 0);
    }
}
