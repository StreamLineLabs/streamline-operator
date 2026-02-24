//! User Controller
//!
//! Reconciles StreamlineUser custom resources to create and manage
//! users and their credentials within Streamline clusters.

use crate::conditions::{
    build_condition, set_condition, CONDITION_FALSE, CONDITION_TRUE, USER_CONDITION_CREDENTIALS_READY,
    USER_CONDITION_READY, USER_FINALIZER,
};
use crate::controllers::error_policy_backoff;
use crate::crd::{StreamlineCluster, StreamlineUser, UserPhase, UserStatus};
use crate::error::{OperatorError, Result};
use chrono::Utc;
use futures::StreamExt;
use k8s_openapi::api::core::v1::Secret;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference};
use kube::api::{Api, Patch, PatchParams, PostParams};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher::Config;
use kube::{Client, Resource, ResourceExt};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

/// Context for the user controller
pub struct UserController {
    client: Client,
}

impl UserController {
    /// Create a new user controller
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    /// Run the user controller
    pub async fn run(self: Arc<Self>) -> Result<()> {
        let users: Api<StreamlineUser> = Api::all(self.client.clone());

        info!("Starting StreamlineUser controller");

        Controller::new(users, Config::default())
            .shutdown_on_signal()
            .run(
                |user, ctx| async move { ctx.reconcile(user).await },
                |_user, error, _ctx| {
                    error!("Reconciliation error: {:?}", error);
                    error_policy_backoff(_user, error, _ctx)
                },
                Arc::clone(&self),
            )
            .for_each(|result| async move {
                match result {
                    Ok((obj, _action)) => {
                        info!("Reconciled user: {}", obj.name);
                    }
                    Err(e) => {
                        error!("Reconciliation failed: {:?}", e);
                    }
                }
            })
            .await;

        Ok(())
    }

    /// Reconcile a StreamlineUser
    async fn reconcile(
        &self,
        user: Arc<StreamlineUser>,
    ) -> std::result::Result<Action, OperatorError> {
        let name = user.name_any();
        let namespace = user.namespace().unwrap_or_else(|| "default".to_string());

        info!("Reconciling StreamlineUser {}/{}", namespace, name);

        // Handle deletion with finalizer
        if user.metadata.deletion_timestamp.is_some() {
            return self.handle_deletion(&user, &namespace).await;
        }

        // Ensure finalizer is set
        self.ensure_finalizer(&user, &namespace).await?;

        // Get the referenced cluster
        let clusters: Api<StreamlineCluster> = Api::namespaced(self.client.clone(), &namespace);
        let cluster = match clusters.get(&user.spec.cluster_ref).await {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    "Cluster {} not found for user {}: {}",
                    user.spec.cluster_ref, name, e
                );
                self.update_status_error(
                    &user,
                    &namespace,
                    &format!("Cluster {} not found", user.spec.cluster_ref),
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
                "Cluster {} not ready for user {}",
                user.spec.cluster_ref, name
            );
            self.update_status_pending(&user, &namespace, "Waiting for cluster to be ready")
                .await?;
            return Ok(Action::requeue(Duration::from_secs(10)));
        }

        // Create/update credentials secret if needed
        let credentials_secret = self.reconcile_credentials_secret(&user, &namespace).await?;

        // Create/update user in Streamline cluster
        match self
            .create_or_update_user(&user, &cluster, &credentials_secret)
            .await
        {
            Ok(_) => {
                self.update_status_ready(&user, &namespace, &credentials_secret)
                    .await?;
            }
            Err(e) => {
                error!("Failed to create/update user {}: {}", name, e);
                self.update_status_error(&user, &namespace, &e.to_string())
                    .await?;
                return Ok(Action::requeue(Duration::from_secs(30)));
            }
        }

        Ok(Action::requeue(Duration::from_secs(60)))
    }

    /// Ensure the finalizer is present on the resource
    async fn ensure_finalizer(
        &self,
        user: &StreamlineUser,
        namespace: &str,
    ) -> Result<()> {
        let finalizers = user.metadata.finalizers.as_deref().unwrap_or_default();
        if finalizers.contains(&USER_FINALIZER.to_string()) {
            return Ok(());
        }

        let users: Api<StreamlineUser> = Api::namespaced(self.client.clone(), namespace);
        let patch = serde_json::json!({
            "metadata": {
                "finalizers": [USER_FINALIZER]
            }
        });
        users
            .patch(
                &user.name_any(),
                &PatchParams::apply("streamline-operator").force(),
                &Patch::Apply(&patch),
            )
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        Ok(())
    }

    /// Handle deletion: revoke credentials from Streamline server, then remove finalizer
    async fn handle_deletion(
        &self,
        user: &StreamlineUser,
        namespace: &str,
    ) -> std::result::Result<Action, OperatorError> {
        let name = user.name_any();
        info!("Handling deletion of StreamlineUser {}/{}", namespace, name);

        // Attempt to revoke user credentials from the Streamline cluster
        let clusters: Api<StreamlineCluster> = Api::namespaced(self.client.clone(), namespace);
        if let Ok(cluster) = clusters.get(&user.spec.cluster_ref).await {
            let cluster_name = cluster.name_any();
            let http_endpoint = format!(
                "http://{}-0.{}-headless.{}.svc:{}",
                cluster_name, cluster_name, namespace, cluster.spec.http_port
            );
            info!(
                "Would revoke credentials for user {} from cluster at {}",
                name, http_endpoint
            );
            // TODO: Implement actual HTTP DELETE call to Streamline API
        } else {
            warn!(
                "Cluster {} not found during user deletion, skipping credential revocation",
                user.spec.cluster_ref
            );
        }

        // Remove finalizer
        let users: Api<StreamlineUser> = Api::namespaced(self.client.clone(), namespace);
        let finalizers: Vec<String> = user
            .metadata
            .finalizers
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|f| f.as_str() != USER_FINALIZER)
            .cloned()
            .collect();

        let patch = serde_json::json!({
            "metadata": {
                "finalizers": finalizers
            }
        });
        users
            .patch(
                &name,
                &PatchParams::default(),
                &Patch::Merge(&patch),
            )
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        info!("Finalizer removed for StreamlineUser {}/{}", namespace, name);
        Ok(Action::await_change())
    }

    /// Reconcile credentials secret
    async fn reconcile_credentials_secret(
        &self,
        user: &StreamlineUser,
        namespace: &str,
    ) -> Result<String> {
        let secret_name = format!("{}-credentials", user.name_any());
        let secrets: Api<Secret> = Api::namespaced(self.client.clone(), namespace);

        // Check if secret already exists
        if secrets.get(&secret_name).await.is_ok() {
            return Ok(secret_name);
        }

        // Check if password is provided in spec
        let password = if let Some(creds) = &user.spec.authentication.credentials {
            if let Some(value) = &creds.value {
                value.clone()
            } else if let Some(secret_ref) = &creds.secret_ref {
                // Fetch password from referenced secret
                let ref_secret = secrets.get(&secret_ref.name).await.map_err(|e| {
                    OperatorError::KubeApi(format!(
                        "Failed to get secret {}: {}",
                        secret_ref.name, e
                    ))
                })?;

                let data = ref_secret.data.ok_or_else(|| {
                    OperatorError::Configuration("Referenced secret has no data".to_string())
                })?;

                let password_bytes = data.get(&secret_ref.key).ok_or_else(|| {
                    OperatorError::Configuration(format!(
                        "Key {} not found in secret",
                        secret_ref.key
                    ))
                })?;

                String::from_utf8(password_bytes.0.clone()).map_err(|e| {
                    OperatorError::Configuration(format!("Invalid password encoding: {}", e))
                })?
            } else {
                // Generate random password
                self.generate_random_password()
            }
        } else {
            // Generate random password
            self.generate_random_password()
        };

        // Create owner reference
        let owner_ref = OwnerReference {
            api_version: StreamlineUser::api_version(&()).to_string(),
            kind: StreamlineUser::kind(&()).to_string(),
            name: user.name_any(),
            uid: user.metadata.uid.clone().unwrap_or_default(),
            controller: Some(true),
            block_owner_deletion: Some(true),
        };

        // Create the credentials secret
        let mut labels = BTreeMap::new();
        labels.insert(
            "app.kubernetes.io/name".to_string(),
            "streamline".to_string(),
        );
        labels.insert(
            "app.kubernetes.io/managed-by".to_string(),
            "streamline-operator".to_string(),
        );
        labels.insert("streamline.io/user".to_string(), user.name_any());

        let mut string_data = BTreeMap::new();
        string_data.insert("username".to_string(), user.name_any());
        string_data.insert("password".to_string(), password);

        let secret = Secret {
            metadata: ObjectMeta {
                name: Some(secret_name.clone()),
                namespace: Some(namespace.to_string()),
                labels: Some(labels),
                owner_references: Some(vec![owner_ref]),
                ..Default::default()
            },
            string_data: Some(string_data),
            type_: Some("Opaque".to_string()),
            ..Default::default()
        };

        secrets
            .create(&PostParams::default(), &secret)
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        Ok(secret_name)
    }

    /// Generate a random password
    fn generate_random_password(&self) -> String {
        use rand::Rng;
        use std::iter;

        const CHARSET: &[u8] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!@#$%^&*";
        let mut rng = rand::thread_rng();
        let password: String = iter::repeat(())
            .map(|()| {
                let idx = rng.gen_range(0..CHARSET.len());
                CHARSET[idx] as char
            })
            .take(24)
            .collect();
        password
    }

    /// Create or update a user in the Streamline cluster
    async fn create_or_update_user(
        &self,
        user: &StreamlineUser,
        cluster: &StreamlineCluster,
        _credentials_secret: &str,
    ) -> Result<()> {
        let namespace = cluster.namespace().unwrap_or_else(|| "default".to_string());
        let cluster_name = cluster.name_any();

        // Build the Streamline HTTP API endpoint
        let http_endpoint = format!(
            "http://{}-0.{}-headless.{}.svc:{}",
            cluster_name, cluster_name, namespace, cluster.spec.http_port
        );

        // Build user configuration
        let user_config = serde_json::json!({
            "username": user.name_any(),
            "authentication": {
                "type": user.spec.authentication.r#type,
            },
            "authorization": {
                "type": user.spec.authorization.r#type,
                "acls": user.spec.authorization.acls,
                "roles": user.spec.authorization.roles,
            },
            "quotas": user.spec.quotas,
        });

        // In a real implementation, we would make an HTTP request to the Streamline API
        info!(
            "Would create/update user {} at {} with config: {:?}",
            user.name_any(),
            http_endpoint,
            user_config
        );

        // TODO: Implement actual HTTP client call to Streamline API

        Ok(())
    }

    /// Update status to ready with Ready and CredentialsReady conditions
    async fn update_status_ready(
        &self,
        user: &StreamlineUser,
        namespace: &str,
        credentials_secret: &str,
    ) -> Result<()> {
        let name = user.name_any();
        let users: Api<StreamlineUser> = Api::namespaced(self.client.clone(), namespace);

        let mut cond_fields = Vec::new();
        set_condition(&mut cond_fields, build_condition(
            USER_CONDITION_READY, CONDITION_TRUE, "UserReady", "User successfully created/updated",
        ));
        set_condition(&mut cond_fields, build_condition(
            USER_CONDITION_CREDENTIALS_READY, CONDITION_TRUE, "CredentialsProvisioned",
            &format!("Credentials stored in secret {}", credentials_secret),
        ));

        let conditions = cond_fields.into_iter().map(|c| c.into_user_condition()).collect();

        let status = UserStatus {
            ready: true,
            phase: UserPhase::Ready,
            username: Some(name.clone()),
            credentials_secret: Some(credentials_secret.to_string()),
            conditions,
            observed_generation: user.metadata.generation,
            last_updated: Some(Utc::now().to_rfc3339()),
            error_message: None,
        };

        let patch = serde_json::json!({ "status": status });
        users
            .patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        Ok(())
    }

    /// Update status to pending
    async fn update_status_pending(
        &self,
        user: &StreamlineUser,
        namespace: &str,
        message: &str,
    ) -> Result<()> {
        let name = user.name_any();
        let users: Api<StreamlineUser> = Api::namespaced(self.client.clone(), namespace);

        let mut cond_fields = Vec::new();
        set_condition(&mut cond_fields, build_condition(
            USER_CONDITION_READY, CONDITION_FALSE, "Pending", message,
        ));
        set_condition(&mut cond_fields, build_condition(
            USER_CONDITION_CREDENTIALS_READY, CONDITION_FALSE, "WaitingForCluster",
            "Credentials cannot be provisioned until cluster is ready",
        ));

        let conditions = cond_fields.into_iter().map(|c| c.into_user_condition()).collect();

        let status = UserStatus {
            ready: false,
            phase: UserPhase::Pending,
            username: None,
            credentials_secret: None,
            conditions,
            observed_generation: user.metadata.generation,
            last_updated: Some(Utc::now().to_rfc3339()),
            error_message: None,
        };

        let patch = serde_json::json!({ "status": status });
        users
            .patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        Ok(())
    }

    /// Update status to error
    async fn update_status_error(
        &self,
        user: &StreamlineUser,
        namespace: &str,
        error_message: &str,
    ) -> Result<()> {
        let name = user.name_any();
        let users: Api<StreamlineUser> = Api::namespaced(self.client.clone(), namespace);

        let mut cond_fields = Vec::new();
        set_condition(&mut cond_fields, build_condition(
            USER_CONDITION_READY, CONDITION_FALSE, "Error", error_message,
        ));
        set_condition(&mut cond_fields, build_condition(
            USER_CONDITION_CREDENTIALS_READY, CONDITION_FALSE, "ProvisioningFailed", error_message,
        ));

        let conditions = cond_fields.into_iter().map(|c| c.into_user_condition()).collect();

        let status = UserStatus {
            ready: false,
            phase: UserPhase::Failed,
            username: None,
            credentials_secret: None,
            conditions,
            observed_generation: user.metadata.generation,
            last_updated: Some(Utc::now().to_rfc3339()),
            error_message: Some(error_message.to_string()),
        };

        let patch = serde_json::json!({ "status": status });
        users
            .patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_user_controller() {
        // Controller tests require k8s cluster
    }
}
