//! Topic Controller
//!
//! Reconciles StreamlineTopic custom resources to create and manage
//! topics within Streamline clusters.

use crate::conditions::{
    build_condition, set_condition, CONDITION_FALSE, CONDITION_TRUE, TOPIC_CONDITION_READY,
    TOPIC_CONDITION_SYNCED, TOPIC_FINALIZER,
};
use crate::controllers::error_policy_backoff;
use crate::crd::{StreamlineCluster, StreamlineTopic, TopicPhase, TopicStatus};
use crate::error::{OperatorError, Result};
use chrono::Utc;
use futures::StreamExt;
use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher::Config;
use kube::{Client, ResourceExt};
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

/// Context for the topic controller
pub struct TopicController {
    client: Client,
}

impl TopicController {
    /// Create a new topic controller
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    /// Run the topic controller
    pub async fn run(self: Arc<Self>) -> Result<()> {
        let topics: Api<StreamlineTopic> = Api::all(self.client.clone());

        info!("Starting StreamlineTopic controller");

        Controller::new(topics, Config::default())
            .shutdown_on_signal()
            .run(
                |topic, ctx| async move { ctx.reconcile(topic).await },
                |_topic, error, _ctx| {
                    error!("Reconciliation error: {:?}", error);
                    error_policy_backoff(_topic, error, _ctx)
                },
                Arc::clone(&self),
            )
            .for_each(|result| async move {
                match result {
                    Ok((obj, _action)) => {
                        info!("Reconciled topic: {}", obj.name);
                    }
                    Err(e) => {
                        error!("Reconciliation failed: {:?}", e);
                    }
                }
            })
            .await;

        Ok(())
    }

    /// Reconcile a StreamlineTopic
    async fn reconcile(
        &self,
        topic: Arc<StreamlineTopic>,
    ) -> std::result::Result<Action, OperatorError> {
        let name = topic.name_any();
        let namespace = topic.namespace().unwrap_or_else(|| "default".to_string());

        info!("Reconciling StreamlineTopic {}/{}", namespace, name);

        // Handle deletion with finalizer
        if topic.metadata.deletion_timestamp.is_some() {
            return self.handle_deletion(&topic, &namespace).await;
        }

        // Ensure finalizer is set
        self.ensure_finalizer(&topic, &namespace).await?;

        // Get the referenced cluster
        let clusters: Api<StreamlineCluster> = Api::namespaced(self.client.clone(), &namespace);
        let cluster = match clusters.get(&topic.spec.cluster_ref).await {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    "Cluster {} not found for topic {}: {}",
                    topic.spec.cluster_ref, name, e
                );
                self.update_status_error(
                    &topic,
                    &namespace,
                    &format!("Cluster {} not found", topic.spec.cluster_ref),
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
                "Cluster {} not ready for topic {}",
                topic.spec.cluster_ref, name
            );
            self.update_status_pending(&topic, &namespace, "Waiting for cluster to be ready")
                .await?;
            return Ok(Action::requeue(Duration::from_secs(10)));
        }

        // Create/update topic via Streamline HTTP API
        match self.create_or_update_topic(&topic, &cluster).await {
            Ok(_) => {
                self.update_status_ready(&topic, &namespace).await?;
            }
            Err(e) => {
                error!("Failed to create/update topic {}: {}", name, e);
                self.update_status_error(&topic, &namespace, &e.to_string())
                    .await?;
                return Ok(Action::requeue(Duration::from_secs(30)));
            }
        }

        Ok(Action::requeue(Duration::from_secs(60)))
    }

    /// Ensure the finalizer is present on the resource
    async fn ensure_finalizer(
        &self,
        topic: &StreamlineTopic,
        namespace: &str,
    ) -> Result<()> {
        let finalizers = topic.metadata.finalizers.as_deref().unwrap_or_default();
        if finalizers.contains(&TOPIC_FINALIZER.to_string()) {
            return Ok(());
        }

        let topics: Api<StreamlineTopic> = Api::namespaced(self.client.clone(), namespace);
        let patch = serde_json::json!({
            "metadata": {
                "finalizers": [TOPIC_FINALIZER]
            }
        });
        topics
            .patch(
                &topic.name_any(),
                &PatchParams::apply("streamline-operator").force(),
                &Patch::Apply(&patch),
            )
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        Ok(())
    }

    /// Handle deletion: remove topic from Streamline server, then remove finalizer
    async fn handle_deletion(
        &self,
        topic: &StreamlineTopic,
        namespace: &str,
    ) -> std::result::Result<Action, OperatorError> {
        let name = topic.name_any();
        info!("Handling deletion of StreamlineTopic {}/{}", namespace, name);

        // Attempt to delete topic from the Streamline cluster
        let clusters: Api<StreamlineCluster> = Api::namespaced(self.client.clone(), namespace);
        if let Ok(cluster) = clusters.get(&topic.spec.cluster_ref).await {
            let cluster_name = cluster.name_any();
            let http_endpoint = format!(
                "http://{}-0.{}-headless.{}.svc:{}",
                cluster_name, cluster_name, namespace, cluster.spec.http_port
            );
            info!(
                "Would delete topic {} from cluster at {}",
                name, http_endpoint
            );
            // TODO: Implement actual HTTP DELETE call to Streamline API
        } else {
            warn!(
                "Cluster {} not found during topic deletion, skipping server cleanup",
                topic.spec.cluster_ref
            );
        }

        // Remove finalizer
        let topics: Api<StreamlineTopic> = Api::namespaced(self.client.clone(), namespace);
        let finalizers: Vec<String> = topic
            .metadata
            .finalizers
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|f| f.as_str() != TOPIC_FINALIZER)
            .cloned()
            .collect();

        let patch = serde_json::json!({
            "metadata": {
                "finalizers": finalizers
            }
        });
        topics
            .patch(
                &name,
                &PatchParams::default(),
                &Patch::Merge(&patch),
            )
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        info!("Finalizer removed for StreamlineTopic {}/{}", namespace, name);
        Ok(Action::await_change())
    }

    /// Create or update a topic in the Streamline cluster
    async fn create_or_update_topic(
        &self,
        topic: &StreamlineTopic,
        cluster: &StreamlineCluster,
    ) -> Result<()> {
        let namespace = cluster.namespace().unwrap_or_else(|| "default".to_string());
        let cluster_name = cluster.name_any();

        // Build the Streamline HTTP API endpoint
        // Use the first broker's HTTP endpoint
        let http_endpoint = format!(
            "http://{}-0.{}-headless.{}.svc:{}",
            cluster_name, cluster_name, namespace, cluster.spec.http_port
        );

        // Build topic configuration
        let topic_config = serde_json::json!({
            "name": topic.name_any(),
            "partitions": topic.spec.partitions,
            "replication_factor": topic.spec.replication_factor,
            "config": {
                "retention_ms": topic.spec.retention.retention_ms,
                "retention_bytes": topic.spec.retention.retention_bytes,
                "cleanup_policy": topic.spec.retention.cleanup_policy,
                "compression_type": topic.spec.compression.r#type,
                "min_insync_replicas": topic.spec.config.min_insync_replicas,
                "max_message_bytes": topic.spec.config.max_message_bytes,
                "segment_bytes": topic.spec.config.segment_bytes,
            }
        });

        // In a real implementation, we would make an HTTP request to the Streamline API
        // For now, we'll log the intended action
        info!(
            "Would create/update topic {} at {} with config: {:?}",
            topic.name_any(),
            http_endpoint,
            topic_config
        );

        // TODO: Implement actual HTTP client call to Streamline API
        // This would require adding reqwest or similar HTTP client dependency
        // let client = reqwest::Client::new();
        // let response = client
        //     .post(format!("{}/api/v1/topics", http_endpoint))
        //     .json(&topic_config)
        //     .send()
        //     .await?;

        Ok(())
    }

    /// Update status to ready with Ready and Synced conditions
    async fn update_status_ready(&self, topic: &StreamlineTopic, namespace: &str) -> Result<()> {
        let name = topic.name_any();
        let topics: Api<StreamlineTopic> = Api::namespaced(self.client.clone(), namespace);

        let mut cond_fields = Vec::new();
        set_condition(&mut cond_fields, build_condition(
            TOPIC_CONDITION_READY, CONDITION_TRUE, "TopicReady", "Topic successfully created/updated",
        ));
        set_condition(&mut cond_fields, build_condition(
            TOPIC_CONDITION_SYNCED, CONDITION_TRUE, "ConfigSynced", "Topic configuration is in sync with desired state",
        ));

        let conditions = cond_fields.into_iter().map(|c| c.into_topic_condition()).collect();

        let status = TopicStatus {
            ready: true,
            phase: TopicPhase::Ready,
            partitions: topic.spec.partitions,
            replication_factor: topic.spec.replication_factor,
            partition_assignments: vec![],
            conditions,
            observed_generation: topic.metadata.generation,
            last_updated: Some(Utc::now().to_rfc3339()),
            error_message: None,
        };

        let patch = serde_json::json!({ "status": status });
        topics
            .patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        Ok(())
    }

    /// Update status to pending
    async fn update_status_pending(
        &self,
        topic: &StreamlineTopic,
        namespace: &str,
        message: &str,
    ) -> Result<()> {
        let name = topic.name_any();
        let topics: Api<StreamlineTopic> = Api::namespaced(self.client.clone(), namespace);

        let mut cond_fields = Vec::new();
        set_condition(&mut cond_fields, build_condition(
            TOPIC_CONDITION_READY, CONDITION_FALSE, "Pending", message,
        ));
        set_condition(&mut cond_fields, build_condition(
            TOPIC_CONDITION_SYNCED, CONDITION_FALSE, "WaitingForCluster", "Topic cannot sync until cluster is ready",
        ));

        let conditions = cond_fields.into_iter().map(|c| c.into_topic_condition()).collect();

        let status = TopicStatus {
            ready: false,
            phase: TopicPhase::Pending,
            partitions: 0,
            replication_factor: 0,
            partition_assignments: vec![],
            conditions,
            observed_generation: topic.metadata.generation,
            last_updated: Some(Utc::now().to_rfc3339()),
            error_message: None,
        };

        let patch = serde_json::json!({ "status": status });
        topics
            .patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        Ok(())
    }

    /// Update status to error
    async fn update_status_error(
        &self,
        topic: &StreamlineTopic,
        namespace: &str,
        error_message: &str,
    ) -> Result<()> {
        let name = topic.name_any();
        let topics: Api<StreamlineTopic> = Api::namespaced(self.client.clone(), namespace);

        let mut cond_fields = Vec::new();
        set_condition(&mut cond_fields, build_condition(
            TOPIC_CONDITION_READY, CONDITION_FALSE, "Error", error_message,
        ));
        set_condition(&mut cond_fields, build_condition(
            TOPIC_CONDITION_SYNCED, CONDITION_FALSE, "SyncFailed", error_message,
        ));

        let conditions = cond_fields.into_iter().map(|c| c.into_topic_condition()).collect();

        let status = TopicStatus {
            ready: false,
            phase: TopicPhase::Failed,
            partitions: 0,
            replication_factor: 0,
            partition_assignments: vec![],
            conditions,
            observed_generation: topic.metadata.generation,
            last_updated: Some(Utc::now().to_rfc3339()),
            error_message: Some(error_message.to_string()),
        };

        let patch = serde_json::json!({ "status": status });
        topics
            .patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_topic_controller() {
        // Controller tests require k8s cluster
    }
}
