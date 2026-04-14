//! StreamlineBranch reconciliation controller (M5 P4).
//! Manages branch lifecycle via the admin API.

use crate::conditions::{
    build_condition, set_condition, CONDITION_FALSE, CONDITION_TRUE, BRANCH_FINALIZER,
};
use crate::controllers::error_policy_backoff;
use crate::crd::{
    BranchCondition, BranchPhase, BranchStatus, StreamlineBranch, StreamlineCluster,
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

// Branch-specific condition types
const BRANCH_CONDITION_ACTIVE: &str = "Active";
const BRANCH_CONDITION_FROZEN: &str = "Frozen";

/// Context for the branch controller
pub struct BranchController {
    client: Client,
    http_client: reqwest::Client,
}

impl BranchController {
    /// Create a new branch controller
    pub fn new(client: Client, http_client: reqwest::Client) -> Self {
        Self { client, http_client }
    }

    /// Run the branch controller
    pub async fn run(self: Arc<Self>) -> Result<()> {
        let branches: Api<StreamlineBranch> = Api::all(self.client.clone());

        info!("Starting StreamlineBranch controller");

        Controller::new(branches, Config::default())
            .shutdown_on_signal()
            .run(
                |branch, ctx| async move { ctx.reconcile(branch).await },
                |_branch, error, _ctx| {
                    error!("Reconciliation error: {:?}", error);
                    crate::metrics::get().inc_error("branch");
                    error_policy_backoff(_branch, error, _ctx)
                },
                Arc::clone(&self),
            )
            .for_each(|result| async move {
                match result {
                    Ok((obj, _action)) => {
                        info!("Reconciled branch: {}", obj.name);
                    }
                    Err(e) => {
                        error!("Reconciliation failed: {:?}", e);
                    }
                }
            })
            .await;

        Ok(())
    }

    /// Reconcile a StreamlineBranch
    async fn reconcile(
        &self,
        branch: Arc<StreamlineBranch>,
    ) -> std::result::Result<Action, OperatorError> {
        crate::metrics::get().inc_reconcile("branch");
        let _timer = crate::metrics::get().start_timer();
        let name = branch.name_any();
        let namespace = branch.namespace().unwrap_or_else(|| "default".to_string());

        info!("Reconciling StreamlineBranch {}/{}", namespace, name);

        // Handle deletion with finalizer
        if branch.metadata.deletion_timestamp.is_some() {
            return self.handle_deletion(&branch, &namespace).await;
        }

        // Ensure finalizer is set
        self.ensure_finalizer(&branch, &namespace).await?;

        // Get the referenced cluster
        let clusters: Api<StreamlineCluster> = Api::namespaced(self.client.clone(), &namespace);
        let cluster = match clusters.get(&branch.spec.cluster_ref).await {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    "Cluster {} not found for branch {}: {}",
                    branch.spec.cluster_ref, name, e
                );
                self.update_status_error(
                    &branch,
                    &namespace,
                    &format!("Cluster {} not found", branch.spec.cluster_ref),
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
                "Cluster {} not ready for branch {}",
                branch.spec.cluster_ref, name
            );
            self.update_status_pending(&branch, &namespace, "Waiting for cluster to be ready")
                .await?;
            return Ok(Action::requeue(Duration::from_secs(10)));
        }

        // Build HTTP endpoint from cluster
        let cluster_name = cluster.name_any();
        let http_endpoint = format!(
            "http://{}-0.{}-headless.{}.svc:{}",
            cluster_name, cluster_name, namespace, cluster.spec.http_port
        );

        // Create the branch
        match self.create_branch(&branch, &http_endpoint).await {
            Ok(created_at_ms) => {
                self.update_status_active(&branch, &namespace, created_at_ms)
                    .await?;
            }
            Err(e) => {
                error!("Failed to create branch {}: {}", name, e);
                self.update_status_error(&branch, &namespace, &e.to_string())
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
        branch: &StreamlineBranch,
        namespace: &str,
    ) -> Result<()> {
        let finalizers = branch.metadata.finalizers.as_deref().unwrap_or_default();
        if finalizers.contains(&BRANCH_FINALIZER.to_string()) {
            return Ok(());
        }

        let branches: Api<StreamlineBranch> = Api::namespaced(self.client.clone(), namespace);
        let patch = serde_json::json!({
            "metadata": {
                "finalizers": [BRANCH_FINALIZER]
            }
        });
        branches
            .patch(
                &branch.name_any(),
                &PatchParams::apply("streamline-operator").force(),
                &Patch::Apply(&patch),
            )
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        Ok(())
    }

    /// Handle deletion: remove branch from Streamline server, then remove finalizer
    async fn handle_deletion(
        &self,
        branch: &StreamlineBranch,
        namespace: &str,
    ) -> std::result::Result<Action, OperatorError> {
        let name = branch.name_any();
        info!(
            "Handling deletion of StreamlineBranch {}/{}",
            namespace, name
        );

        // Attempt to delete branch from the Streamline cluster
        let clusters: Api<StreamlineCluster> = Api::namespaced(self.client.clone(), namespace);
        if let Ok(cluster) = clusters.get(&branch.spec.cluster_ref).await {
            let cluster_name = cluster.name_any();
            let http_endpoint = format!(
                "http://{}-0.{}-headless.{}.svc:{}",
                cluster_name, cluster_name, namespace, cluster.spec.http_port
            );
            info!(
                "Deleting branch {} from cluster at {}",
                name, http_endpoint
            );
            if let Err(e) = self
                .http_client
                .delete(format!("{}/api/v1/branches/{}", http_endpoint, name))
                .send()
                .await
            {
                warn!("Failed to delete branch from cluster API: {}", e);
            }
        } else {
            warn!(
                "Cluster {} not found during branch deletion, skipping server cleanup",
                branch.spec.cluster_ref
            );
        }

        // Remove finalizer
        let branches: Api<StreamlineBranch> = Api::namespaced(self.client.clone(), namespace);
        let finalizers: Vec<String> = branch
            .metadata
            .finalizers
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|f| f.as_str() != BRANCH_FINALIZER)
            .cloned()
            .collect();

        let patch = serde_json::json!({
            "metadata": {
                "finalizers": finalizers
            }
        });
        branches
            .patch(
                &name,
                &PatchParams::default(),
                &Patch::Merge(&patch),
            )
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        info!(
            "Finalizer removed for StreamlineBranch {}/{}",
            namespace, name
        );
        Ok(Action::await_change())
    }

    /// Create a branch in the Streamline cluster
    async fn create_branch(
        &self,
        branch: &StreamlineBranch,
        http_endpoint: &str,
    ) -> Result<Option<i64>> {
        let mut body = serde_json::json!({
            "name": branch.name_any(),
        });

        if let Some(parent) = &branch.spec.parent {
            body["parent"] = serde_json::json!(parent);
        }

        if let Some(desc) = &branch.spec.description {
            body["description"] = serde_json::json!(desc);
        }

        let response = self
            .http_client
            .post(format!("{}/api/v1/branches", http_endpoint))
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                OperatorError::Internal(format!(
                    "HTTP request to create branch {} failed: {}",
                    branch.name_any(),
                    e
                ))
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let resp_body = response.text().await.unwrap_or_default();
            // 409 Conflict means branch already exists — treat as success for idempotency
            if status.as_u16() != 409 {
                return Err(OperatorError::Internal(format!(
                    "Failed to create branch {} (HTTP {}): {}",
                    branch.name_any(),
                    status,
                    resp_body
                )));
            }
            return Ok(None);
        }

        // Try to extract created_at_ms from response
        let created_at_ms = response
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v.get("created_at_ms").and_then(|c| c.as_i64()));

        Ok(created_at_ms)
    }

    /// Update status to active
    async fn update_status_active(
        &self,
        branch: &StreamlineBranch,
        namespace: &str,
        created_at_ms: Option<i64>,
    ) -> Result<()> {
        let name = branch.name_any();
        let branches: Api<StreamlineBranch> = Api::namespaced(self.client.clone(), namespace);

        let mut cond_fields = Vec::new();
        set_condition(
            &mut cond_fields,
            build_condition(
                BRANCH_CONDITION_ACTIVE,
                CONDITION_TRUE,
                "BranchActive",
                "Branch is active and ready for reads",
            ),
        );
        set_condition(
            &mut cond_fields,
            build_condition(
                BRANCH_CONDITION_FROZEN,
                CONDITION_FALSE,
                "NotFrozen",
                "Branch is not frozen",
            ),
        );

        let conditions: Vec<BranchCondition> = cond_fields
            .into_iter()
            .map(|c| BranchCondition {
                r#type: c.condition_type,
                status: c.status,
                last_transition_time: c.last_transition_time,
                reason: c.reason,
                message: c.message,
            })
            .collect();

        let status = BranchStatus {
            phase: BranchPhase::Ready,
            ready: true,
            created_at_ms,
            message: Some("Branch created successfully".to_string()),
            conditions,
        };

        let patch = serde_json::json!({ "status": status });
        branches
            .patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        Ok(())
    }

    /// Update status to pending
    async fn update_status_pending(
        &self,
        branch: &StreamlineBranch,
        namespace: &str,
        message: &str,
    ) -> Result<()> {
        let name = branch.name_any();
        let branches: Api<StreamlineBranch> = Api::namespaced(self.client.clone(), namespace);

        let mut cond_fields = Vec::new();
        set_condition(
            &mut cond_fields,
            build_condition(
                BRANCH_CONDITION_ACTIVE,
                CONDITION_FALSE,
                "Pending",
                message,
            ),
        );

        let conditions: Vec<BranchCondition> = cond_fields
            .into_iter()
            .map(|c| BranchCondition {
                r#type: c.condition_type,
                status: c.status,
                last_transition_time: c.last_transition_time,
                reason: c.reason,
                message: c.message,
            })
            .collect();

        let status = BranchStatus {
            phase: BranchPhase::Pending,
            ready: false,
            created_at_ms: None,
            message: Some(message.to_string()),
            conditions,
        };

        let patch = serde_json::json!({ "status": status });
        branches
            .patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        Ok(())
    }

    /// Update status to error
    async fn update_status_error(
        &self,
        branch: &StreamlineBranch,
        namespace: &str,
        error_message: &str,
    ) -> Result<()> {
        let name = branch.name_any();
        let branches: Api<StreamlineBranch> = Api::namespaced(self.client.clone(), namespace);

        let mut cond_fields = Vec::new();
        set_condition(
            &mut cond_fields,
            build_condition(
                BRANCH_CONDITION_ACTIVE,
                CONDITION_FALSE,
                "Error",
                error_message,
            ),
        );

        let conditions: Vec<BranchCondition> = cond_fields
            .into_iter()
            .map(|c| BranchCondition {
                r#type: c.condition_type,
                status: c.status,
                last_transition_time: c.last_transition_time,
                reason: c.reason,
                message: c.message,
            })
            .collect();

        let status = BranchStatus {
            phase: BranchPhase::Failed,
            ready: false,
            created_at_ms: None,
            message: Some(error_message.to_string()),
            conditions,
        };

        let patch = serde_json::json!({ "status": status });
        branches
            .patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_branch_controller() {
        // Controller tests require k8s cluster
    }
}
