//! StreamlineContract reconciliation controller (M4 P4).
//! Applies/removes contracts on Streamline topics via the admin API.

use crate::conditions::{
    build_condition, set_condition, CONDITION_FALSE, CONDITION_TRUE, CONTRACT_FINALIZER,
};
use crate::controllers::error_policy_backoff;
use crate::crd::{
    ContractCondition, ContractPhase, ContractStatus, StreamlineCluster, StreamlineContract,
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

// Contract-specific condition types
const CONTRACT_CONDITION_APPLIED: &str = "Applied";
const CONTRACT_CONDITION_VALIDATED: &str = "Validated";

/// Context for the contract controller
pub struct ContractController {
    client: Client,
    http_client: reqwest::Client,
}

impl ContractController {
    /// Create a new contract controller
    pub fn new(client: Client, http_client: reqwest::Client) -> Self {
        Self { client, http_client }
    }

    /// Run the contract controller
    pub async fn run(self: Arc<Self>) -> Result<()> {
        let contracts: Api<StreamlineContract> = Api::all(self.client.clone());

        info!("Starting StreamlineContract controller");

        Controller::new(contracts, Config::default())
            .shutdown_on_signal()
            .run(
                |contract, ctx| async move { ctx.reconcile(contract).await },
                |_contract, error, _ctx| {
                    error!("Reconciliation error: {:?}", error);
                    crate::metrics::get().inc_error("contract");
                    error_policy_backoff(_contract, error, _ctx)
                },
                Arc::clone(&self),
            )
            .for_each(|result| async move {
                match result {
                    Ok((obj, _action)) => {
                        info!("Reconciled contract: {}", obj.name);
                    }
                    Err(e) => {
                        error!("Reconciliation failed: {:?}", e);
                    }
                }
            })
            .await;

        Ok(())
    }

    /// Reconcile a StreamlineContract
    async fn reconcile(
        &self,
        contract: Arc<StreamlineContract>,
    ) -> std::result::Result<Action, OperatorError> {
        crate::metrics::get().inc_reconcile("contract");
        let _timer = crate::metrics::get().start_timer();
        let name = contract.name_any();
        let namespace = contract.namespace().unwrap_or_else(|| "default".to_string());

        info!("Reconciling StreamlineContract {}/{}", namespace, name);

        // Handle deletion with finalizer
        if contract.metadata.deletion_timestamp.is_some() {
            return self.handle_deletion(&contract, &namespace).await;
        }

        // Ensure finalizer is set
        self.ensure_finalizer(&contract, &namespace).await?;

        // Get the referenced cluster
        let clusters: Api<StreamlineCluster> = Api::namespaced(self.client.clone(), &namespace);
        let cluster = match clusters.get(&contract.spec.cluster_ref).await {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    "Cluster {} not found for contract {}: {}",
                    contract.spec.cluster_ref, name, e
                );
                self.update_status_error(
                    &contract,
                    &namespace,
                    &format!("Cluster {} not found", contract.spec.cluster_ref),
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
                "Cluster {} not ready for contract {}",
                contract.spec.cluster_ref, name
            );
            self.update_status_pending(&contract, &namespace, "Waiting for cluster to be ready")
                .await?;
            return Ok(Action::requeue(Duration::from_secs(10)));
        }

        // Build HTTP endpoint from cluster
        let cluster_name = cluster.name_any();
        let http_endpoint = format!(
            "http://{}-0.{}-headless.{}.svc:{}",
            cluster_name, cluster_name, namespace, cluster.spec.http_port
        );

        // Validate the contract schema
        match self.validate_contract(&contract, &http_endpoint).await {
            Ok(_) => {
                info!("Contract {} validated successfully", name);
            }
            Err(e) => {
                error!("Contract {} validation failed: {}", name, e);
                self.update_status_error(&contract, &namespace, &e.to_string())
                    .await?;
                return Ok(Action::requeue(Duration::from_secs(30)));
            }
        }

        // Apply the contract
        match self.apply_contract(&contract, &http_endpoint).await {
            Ok(bound_topics) => {
                self.update_status_applied(&contract, &namespace, bound_topics)
                    .await?;
            }
            Err(e) => {
                error!("Failed to apply contract {}: {}", name, e);
                self.update_status_error(&contract, &namespace, &e.to_string())
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
        contract: &StreamlineContract,
        namespace: &str,
    ) -> Result<()> {
        let finalizers = contract.metadata.finalizers.as_deref().unwrap_or_default();
        if finalizers.contains(&CONTRACT_FINALIZER.to_string()) {
            return Ok(());
        }

        let contracts: Api<StreamlineContract> = Api::namespaced(self.client.clone(), namespace);
        let patch = serde_json::json!({
            "metadata": {
                "finalizers": [CONTRACT_FINALIZER]
            }
        });
        contracts
            .patch(
                &contract.name_any(),
                &PatchParams::apply("streamline-operator").force(),
                &Patch::Apply(&patch),
            )
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        Ok(())
    }

    /// Handle deletion: remove contract from Streamline server, then remove finalizer
    async fn handle_deletion(
        &self,
        contract: &StreamlineContract,
        namespace: &str,
    ) -> std::result::Result<Action, OperatorError> {
        let name = contract.name_any();
        info!(
            "Handling deletion of StreamlineContract {}/{}",
            namespace, name
        );

        // Attempt to delete contract from the Streamline cluster
        let clusters: Api<StreamlineCluster> = Api::namespaced(self.client.clone(), namespace);
        if let Ok(cluster) = clusters.get(&contract.spec.cluster_ref).await {
            let cluster_name = cluster.name_any();
            let http_endpoint = format!(
                "http://{}-0.{}-headless.{}.svc:{}",
                cluster_name, cluster_name, namespace, cluster.spec.http_port
            );
            info!(
                "Deleting contract {} from cluster at {}",
                name, http_endpoint
            );
            if let Err(e) = self
                .http_client
                .delete(format!("{}/api/v1/contracts/{}", http_endpoint, name))
                .send()
                .await
            {
                warn!("Failed to delete contract from cluster API: {}", e);
            }
        } else {
            warn!(
                "Cluster {} not found during contract deletion, skipping server cleanup",
                contract.spec.cluster_ref
            );
        }

        // Remove finalizer
        let contracts: Api<StreamlineContract> = Api::namespaced(self.client.clone(), namespace);
        let finalizers: Vec<String> = contract
            .metadata
            .finalizers
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|f| f.as_str() != CONTRACT_FINALIZER)
            .cloned()
            .collect();

        let patch = serde_json::json!({
            "metadata": {
                "finalizers": finalizers
            }
        });
        contracts
            .patch(
                &name,
                &PatchParams::default(),
                &Patch::Merge(&patch),
            )
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        info!(
            "Finalizer removed for StreamlineContract {}/{}",
            namespace, name
        );
        Ok(Action::await_change())
    }

    /// Validate a contract against the Streamline API
    async fn validate_contract(
        &self,
        contract: &StreamlineContract,
        http_endpoint: &str,
    ) -> Result<()> {
        let body = serde_json::json!({
            "name": contract.name_any(),
            "schema": contract.spec.schema_json,
            "compatibility": contract.spec.compatibility,
            "bind_topics": contract.spec.bind_topics,
        });

        let response = self
            .http_client
            .post(format!("{}/api/v1/contracts/validate", http_endpoint))
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                OperatorError::Internal(format!(
                    "HTTP request to validate contract {} failed: {}",
                    contract.name_any(),
                    e
                ))
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(OperatorError::Reconciliation(format!(
                "Contract validation failed (HTTP {}): {}",
                status, body
            )));
        }

        Ok(())
    }

    /// Apply a contract to the Streamline cluster
    async fn apply_contract(
        &self,
        contract: &StreamlineContract,
        http_endpoint: &str,
    ) -> Result<Vec<String>> {
        let body = serde_json::json!({
            "name": contract.name_any(),
            "schema": contract.spec.schema_json,
            "compatibility": contract.spec.compatibility,
            "bind_topics": contract.spec.bind_topics,
        });

        let response = self
            .http_client
            .post(format!("{}/api/v1/contracts", http_endpoint))
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                OperatorError::Internal(format!(
                    "HTTP request to apply contract {} failed: {}",
                    contract.name_any(),
                    e
                ))
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            // 409 Conflict means contract already exists — treat as success for idempotency
            if status.as_u16() != 409 {
                return Err(OperatorError::Internal(format!(
                    "Failed to apply contract {} (HTTP {}): {}",
                    contract.name_any(),
                    status,
                    body
                )));
            }
        }

        // Return bound topics from the spec as confirmation
        Ok(contract.spec.bind_topics.clone())
    }

    /// Update status to applied with Applied and Validated conditions
    async fn update_status_applied(
        &self,
        contract: &StreamlineContract,
        namespace: &str,
        bound_topics: Vec<String>,
    ) -> Result<()> {
        let name = contract.name_any();
        let contracts: Api<StreamlineContract> = Api::namespaced(self.client.clone(), namespace);

        let mut cond_fields = Vec::new();
        set_condition(
            &mut cond_fields,
            build_condition(
                CONTRACT_CONDITION_APPLIED,
                CONDITION_TRUE,
                "ContractApplied",
                "Contract successfully applied to the cluster",
            ),
        );
        set_condition(
            &mut cond_fields,
            build_condition(
                CONTRACT_CONDITION_VALIDATED,
                CONDITION_TRUE,
                "SchemaValid",
                "Contract schema validated successfully",
            ),
        );

        let conditions: Vec<ContractCondition> = cond_fields
            .into_iter()
            .map(|c| ContractCondition {
                r#type: c.condition_type,
                status: c.status,
                last_transition_time: c.last_transition_time,
                reason: c.reason,
                message: c.message,
            })
            .collect();

        let status = ContractStatus {
            phase: ContractPhase::Active,
            registered: true,
            bound_topics,
            message: Some("Contract applied successfully".to_string()),
            conditions,
        };

        let patch = serde_json::json!({ "status": status });
        contracts
            .patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        Ok(())
    }

    /// Update status to pending
    async fn update_status_pending(
        &self,
        contract: &StreamlineContract,
        namespace: &str,
        message: &str,
    ) -> Result<()> {
        let name = contract.name_any();
        let contracts: Api<StreamlineContract> = Api::namespaced(self.client.clone(), namespace);

        let mut cond_fields = Vec::new();
        set_condition(
            &mut cond_fields,
            build_condition(
                CONTRACT_CONDITION_APPLIED,
                CONDITION_FALSE,
                "Pending",
                message,
            ),
        );
        set_condition(
            &mut cond_fields,
            build_condition(
                CONTRACT_CONDITION_VALIDATED,
                CONDITION_FALSE,
                "WaitingForCluster",
                "Contract cannot be validated until cluster is ready",
            ),
        );

        let conditions: Vec<ContractCondition> = cond_fields
            .into_iter()
            .map(|c| ContractCondition {
                r#type: c.condition_type,
                status: c.status,
                last_transition_time: c.last_transition_time,
                reason: c.reason,
                message: c.message,
            })
            .collect();

        let status = ContractStatus {
            phase: ContractPhase::Pending,
            registered: false,
            bound_topics: vec![],
            message: Some(message.to_string()),
            conditions,
        };

        let patch = serde_json::json!({ "status": status });
        contracts
            .patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        Ok(())
    }

    /// Update status to error
    async fn update_status_error(
        &self,
        contract: &StreamlineContract,
        namespace: &str,
        error_message: &str,
    ) -> Result<()> {
        let name = contract.name_any();
        let contracts: Api<StreamlineContract> = Api::namespaced(self.client.clone(), namespace);

        let mut cond_fields = Vec::new();
        set_condition(
            &mut cond_fields,
            build_condition(
                CONTRACT_CONDITION_APPLIED,
                CONDITION_FALSE,
                "Error",
                error_message,
            ),
        );
        set_condition(
            &mut cond_fields,
            build_condition(
                CONTRACT_CONDITION_VALIDATED,
                CONDITION_FALSE,
                "ValidationFailed",
                error_message,
            ),
        );

        let conditions: Vec<ContractCondition> = cond_fields
            .into_iter()
            .map(|c| ContractCondition {
                r#type: c.condition_type,
                status: c.status,
                last_transition_time: c.last_transition_time,
                reason: c.reason,
                message: c.message,
            })
            .collect();

        let status = ContractStatus {
            phase: ContractPhase::Failed,
            registered: false,
            bound_topics: vec![],
            message: Some(error_message.to_string()),
            conditions,
        };

        let patch = serde_json::json!({ "status": status });
        contracts
            .patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_contract_controller() {
        // Controller tests require k8s cluster
    }
}
