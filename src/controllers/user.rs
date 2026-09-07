//! User Controller
//!
//! Watches StreamlineUser custom resources and reports, explicitly, that user
//! management is not supported.
//!
//! The Streamline server exposes no user-management API (`/api/v1/users` does
//! not exist), so there is nothing for this controller to create. Earlier
//! versions POSTed to that endpoint and provisioned a Kubernetes Secret with a
//! generated password, which made a `StreamlineUser` look `Ready` while the
//! cluster knew nothing about the user and the credentials in the Secret were
//! never honoured. The controller now fails closed: it publishes an
//! `Unsupported` status and provisions nothing.

use crate::conditions::{
    build_condition, set_condition, ConditionFields, CONDITION_FALSE,
    USER_CONDITION_CREDENTIALS_READY, USER_CONDITION_READY, USER_FINALIZER,
};
use crate::controllers::{error_policy_backoff, WatchScope};
use crate::crd::{StreamlineUser, UserPhase, UserStatus};
use crate::error::{OperatorError, Result};
use chrono::Utc;
use futures::StreamExt;
use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher::Config;
use kube::{Client, ResourceExt};
use std::sync::Arc;
use tracing::{error, info};

/// Condition reason published on every StreamlineUser.
const UNSUPPORTED_REASON: &str = "UnsupportedByServer";

/// Condition/status message published on every StreamlineUser.
const UNSUPPORTED_MESSAGE: &str = "User management is not supported: the Streamline server \
     exposes no user API, so this operator does not create users, credentials, ACLs, or quotas";

/// Context for the user controller
pub struct UserController {
    client: Client,
    scope: WatchScope,
}

impl UserController {
    /// Create a new user controller watching `scope`.
    ///
    /// The HTTP client argument is retained for signature stability; no
    /// Streamline API calls are made for users.
    pub fn new(client: Client, _http_client: reqwest::Client, scope: WatchScope) -> Self {
        Self { client, scope }
    }

    /// Run the user controller
    pub async fn run(self: Arc<Self>) -> Result<()> {
        let users: Api<StreamlineUser> = self.scope.api(self.client.clone());

        info!(
            "Starting StreamlineUser controller (watching {})",
            self.scope.describe()
        );

        Controller::new(users, Config::default())
            .shutdown_on_signal()
            .run(
                |user, ctx| async move { ctx.reconcile(user).await },
                |_user, error, _ctx| {
                    error!("Reconciliation error: {:?}", error);
                    crate::metrics::get().inc_error("user");
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

    /// Reconcile a StreamlineUser.
    ///
    /// User management is **not implemented by the Streamline server**: there is
    /// no `/api/v1/users` endpoint to call, and provisioning a Kubernetes Secret
    /// with a generated password would imply credentials the cluster never
    /// learns about. Rather than silently doing nothing (or reporting `Ready`
    /// for a user that does not exist), the controller publishes an explicit
    /// `Unsupported` status and clears any finalizer left by earlier versions so
    /// deletion is never blocked.
    async fn reconcile(
        &self,
        user: Arc<StreamlineUser>,
    ) -> std::result::Result<Action, OperatorError> {
        crate::metrics::get().inc_reconcile("user");
        let _timer = crate::metrics::get().start_timer();
        let name = user.name_any();
        let namespace = user.namespace().unwrap_or_else(|| "default".to_string());

        info!("Reconciling StreamlineUser {}/{}", namespace, name);

        // Drop any finalizer this operator previously added: nothing is created
        // for a StreamlineUser, so there is nothing to clean up.
        self.remove_finalizer_if_present(&user, &namespace).await?;

        if user.metadata.deletion_timestamp.is_some() {
            info!(
                "StreamlineUser {}/{} is being deleted; no server-side cleanup is required",
                namespace, name
            );
            return Ok(Action::await_change());
        }

        self.update_status_unsupported(&user, &namespace).await?;

        crate::metrics::get().inc_success();
        // Only a spec/CRD change can alter the outcome.
        Ok(Action::await_change())
    }

    /// Remove this operator's finalizer from a user if it is still present.
    async fn remove_finalizer_if_present(
        &self,
        user: &StreamlineUser,
        namespace: &str,
    ) -> Result<()> {
        let existing = user.metadata.finalizers.as_deref().unwrap_or_default();
        if !existing.iter().any(|f| f == USER_FINALIZER) {
            return Ok(());
        }

        let name = user.name_any();
        let users: Api<StreamlineUser> = Api::namespaced(self.client.clone(), namespace);
        let remaining: Vec<String> = existing
            .iter()
            .filter(|f| f.as_str() != USER_FINALIZER)
            .cloned()
            .collect();

        let patch = serde_json::json!({ "metadata": { "finalizers": remaining } });
        users
            .patch(&name, &PatchParams::default(), &Patch::Merge(&patch))
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        info!(
            "Removed stale finalizer from StreamlineUser {}/{}",
            namespace, name
        );
        Ok(())
    }

    /// Publish the `Unsupported` status for a user.
    async fn update_status_unsupported(
        &self,
        user: &StreamlineUser,
        namespace: &str,
    ) -> Result<()> {
        let name = user.name_any();
        let users: Api<StreamlineUser> = Api::namespaced(self.client.clone(), namespace);

        let status = Self::unsupported_status(user);
        if user.status.as_ref() == Some(&status) {
            return Ok(());
        }

        let patch = serde_json::json!({ "status": status });
        users
            .patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        Ok(())
    }

    /// Build the `Unsupported` status object.
    fn unsupported_status(user: &StreamlineUser) -> UserStatus {
        // Seed the condition helper from the current status so an unchanged
        // condition keeps its transition timestamp. Status-only watch events
        // can then produce a byte-for-byte identical desired status and skip
        // the API patch instead of triggering an unbounded reconcile loop.
        let mut cond_fields: Vec<ConditionFields> = user
            .status
            .as_ref()
            .map(|status| {
                status
                    .conditions
                    .iter()
                    .filter(|condition| {
                        condition.r#type == USER_CONDITION_READY
                            || condition.r#type == USER_CONDITION_CREDENTIALS_READY
                    })
                    .map(|condition| ConditionFields {
                        condition_type: condition.r#type.clone(),
                        status: condition.status.clone(),
                        last_transition_time: condition.last_transition_time.clone(),
                        reason: condition.reason.clone(),
                        message: condition.message.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        set_condition(
            &mut cond_fields,
            build_condition(
                USER_CONDITION_READY,
                CONDITION_FALSE,
                UNSUPPORTED_REASON,
                UNSUPPORTED_MESSAGE,
            ),
        );
        set_condition(
            &mut cond_fields,
            build_condition(
                USER_CONDITION_CREDENTIALS_READY,
                CONDITION_FALSE,
                UNSUPPORTED_REASON,
                "No credentials Secret is provisioned: the server cannot be told about \
                 credentials, so generating them would be misleading",
            ),
        );

        let mut status = UserStatus {
            ready: false,
            phase: UserPhase::Unsupported,
            username: None,
            credentials_secret: None,
            conditions: cond_fields
                .into_iter()
                .map(|c| c.into_user_condition())
                .collect(),
            observed_generation: user.metadata.generation,
            last_updated: None,
            error_message: Some(UNSUPPORTED_MESSAGE.to_string()),
        };

        let semantic_change = user.status.as_ref().is_none_or(|current| {
            let mut comparable = current.clone();
            comparable.last_updated = None;
            comparable != status
        });
        status.last_updated = if semantic_change {
            Some(Utc::now().to_rfc3339())
        } else {
            user.status
                .as_ref()
                .and_then(|current| current.last_updated.clone())
        };

        status
    }
}

#[cfg(test)]
mod tests {
    // unwrap/expect are acceptable in tests; the crate-wide lint targets production code.
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::crd::UserSpec;

    fn user() -> StreamlineUser {
        let spec: UserSpec = serde_json::from_str(
            r#"{"clusterRef": "c", "authentication": {"type": "scram-sha512"}}"#,
        )
        .unwrap();
        StreamlineUser::new("app-user", spec)
    }

    #[test]
    fn status_is_unsupported_and_not_ready() {
        let status = UserController::unsupported_status(&user());

        assert!(!status.ready);
        assert_eq!(status.phase, UserPhase::Unsupported);
        assert!(status.error_message.is_some());
    }

    #[test]
    fn no_credentials_secret_is_claimed() {
        let status = UserController::unsupported_status(&user());
        assert!(status.credentials_secret.is_none());
        assert!(status.username.is_none());
    }

    #[test]
    fn conditions_explain_why_the_user_is_not_reconciled() {
        let status = UserController::unsupported_status(&user());

        let ready = status
            .conditions
            .iter()
            .find(|c| c.r#type == USER_CONDITION_READY)
            .expect("Ready condition");
        assert_eq!(ready.status, CONDITION_FALSE);
        assert_eq!(ready.reason.as_deref(), Some(UNSUPPORTED_REASON));

        let credentials = status
            .conditions
            .iter()
            .find(|c| c.r#type == USER_CONDITION_CREDENTIALS_READY)
            .expect("CredentialsReady condition");
        assert_eq!(credentials.status, CONDITION_FALSE);
    }

    #[test]
    fn repeated_unsupported_status_is_stable() {
        let mut resource = user();
        let first = UserController::unsupported_status(&resource);
        resource.status = Some(first.clone());

        let second = UserController::unsupported_status(&resource);
        assert_eq!(second, first);
    }

    #[test]
    fn generation_change_updates_status_without_resetting_condition_transitions() {
        let mut resource = user();
        resource.metadata.generation = Some(1);
        let mut first = UserController::unsupported_status(&resource);
        first.last_updated = Some("2024-01-01T00:00:00Z".to_string());
        let first_transitions: Vec<Option<String>> = first
            .conditions
            .iter()
            .map(|condition| condition.last_transition_time.clone())
            .collect();
        resource.status = Some(first);
        resource.metadata.generation = Some(2);

        let second = UserController::unsupported_status(&resource);
        assert_eq!(second.observed_generation, Some(2));
        assert_ne!(second.last_updated.as_deref(), Some("2024-01-01T00:00:00Z"));
        assert_eq!(
            second
                .conditions
                .iter()
                .map(|condition| condition.last_transition_time.clone())
                .collect::<Vec<_>>(),
            first_transitions
        );
    }
}
