//! Controllers for Streamline Kubernetes Operator
//!
//! Each controller watches its respective CRD and reconciles the actual state
//! with the desired state specified in the custom resources.
//!
//! # Enabled controllers
//!
//! Only `StreamlineCluster`, `StreamlineTopic`, and `StreamlineUser` have a
//! controller. `StreamlineBranch`, `StreamlineContract`, and `StreamlineMemory`
//! deliberately have none — the Streamline server exposes no compatible API for
//! them (see [`crate::crd::generate::Reconciliation`] for the per-kind reason),
//! so their CRDs are schema-only and are neither installed nor RBAC-authorised.
//! Reintroducing a controller here without also changing that reconciliation
//! metadata fails `tests/crd_manifests.rs`.

mod autoscaling;
mod cluster;
pub mod operator_hub;
mod scale_to_zero;
mod topic;
mod user;

pub use autoscaling::{
    AutoScalingConfig, AutoScalingController, CustomMetric, DeleteOutcome, HpaAction,
    MetricTargetSpec, PartitionMetrics, ScalingBehavior, ScalingPolicy, ScalingRecommendation,
    ScalingRules,
};
pub use cluster::ClusterController;
pub use operator_hub::{
    HubConfig, HubOperator, HubStats, InstallStatus, InstalledOperator, IntegrationType,
    OperatorCategory, OperatorHub, BUNDLED_OPERATORS,
};
pub use scale_to_zero::{
    build_activity_snapshot, ClusterActivitySnapshot, KedaConfig, KedaTrigger, ScaleAction,
    ScaleToZeroAction, ScaleToZeroConfig, ScaleToZeroController,
};
pub use topic::TopicController;
pub use user::UserController;

use crate::error::OperatorError;
use kube::api::Api;
use kube::core::NamespaceResourceScope;
use kube::runtime::controller::Action;
use kube::{Client, Resource};
use std::time::Duration;

/// Which namespaces a controller watches.
///
/// Every enabled controller resolves its `Api` through this type, so
/// `--namespace` means the same thing for all of them instead of each
/// controller hard-coding [`Api::all`]. That hard-coding made `--namespace` a
/// no-op: the operator accepted the flag, logged the namespace, and then
/// watched (and required RBAC for) the entire cluster anyway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchScope {
    /// Watch custom resources in every namespace. Requires cluster-wide RBAC.
    AllNamespaces,
    /// Watch custom resources in exactly one namespace. Works with a namespaced
    /// Role/RoleBinding, which is what `deploy/` ships.
    Namespace(String),
}

impl WatchScope {
    /// Build a scope from the `--namespace` flag: empty (or whitespace) means
    /// cluster-wide, anything else names the single namespace to watch.
    pub fn from_flag(namespace: &str) -> Self {
        let trimmed = namespace.trim();
        if trimmed.is_empty() {
            Self::AllNamespaces
        } else {
            Self::Namespace(trimmed.to_string())
        }
    }

    /// The namespace being watched, or `None` for cluster-wide.
    pub fn namespace(&self) -> Option<&str> {
        match self {
            Self::AllNamespaces => None,
            Self::Namespace(ns) => Some(ns),
        }
    }

    /// Human-readable description for startup logging.
    pub fn describe(&self) -> &str {
        match self {
            Self::AllNamespaces => "all namespaces",
            Self::Namespace(ns) => ns,
        }
    }

    /// Resolve the typed `Api` a controller should watch.
    pub fn api<K>(&self, client: Client) -> Api<K>
    where
        K: Resource<Scope = NamespaceResourceScope, DynamicType = ()>,
    {
        match self {
            Self::AllNamespaces => Api::all(client),
            Self::Namespace(ns) => Api::namespaced(client, ns),
        }
    }
}

/// Exponential backoff error policy for controller reconciliation failures.
/// Categorizes errors by severity to choose appropriate retry delays.
pub(crate) fn error_policy_backoff<K>(
    _object: std::sync::Arc<K>,
    error: &OperatorError,
    _ctx: std::sync::Arc<impl std::any::Any + Send + Sync>,
) -> Action {
    let delay_secs = match error {
        // Transient K8s API errors — retry quickly
        OperatorError::KubeApi(_) | OperatorError::Http(_) => 10,
        // Resource not yet available — moderate wait
        OperatorError::NotFound(_) => 15,
        // Reconciliation/state issues — longer wait
        OperatorError::Reconciliation(_) | OperatorError::InvalidState(_) => 30,
        // Config/serialization errors unlikely to self-heal — back off further
        OperatorError::Configuration(_) | OperatorError::Serialization(_) => 60,
        // Internal errors — back off further
        OperatorError::Internal(_) => 60,
    };

    Action::requeue(Duration::from_secs(delay_secs))
}

/// Common trait for all controllers
#[async_trait::async_trait]
pub trait Controller: Send + Sync {
    /// Start the controller's reconciliation loop
    async fn run(&self) -> Result<(), OperatorError>;

    /// Get the controller name for logging
    fn name(&self) -> &'static str;
}

#[cfg(test)]
mod tests {
    // unwrap/expect are acceptable in tests; the crate-wide lint targets production code.
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn empty_namespace_flag_watches_every_namespace() {
        assert_eq!(WatchScope::from_flag(""), WatchScope::AllNamespaces);
        assert_eq!(WatchScope::from_flag("   "), WatchScope::AllNamespaces);
        assert_eq!(WatchScope::from_flag("").namespace(), None);
    }

    #[test]
    fn a_named_namespace_flag_scopes_the_watch() {
        let scope = WatchScope::from_flag("streamline-system");
        assert_eq!(
            scope,
            WatchScope::Namespace("streamline-system".to_string())
        );
        assert_eq!(scope.namespace(), Some("streamline-system"));
        assert_eq!(scope.describe(), "streamline-system");
    }

    #[test]
    fn surrounding_whitespace_is_not_part_of_the_namespace() {
        // `--namespace=$(OPERATOR_NAMESPACE)` can arrive padded from a manifest.
        assert_eq!(
            WatchScope::from_flag(" streamline-system\n"),
            WatchScope::Namespace("streamline-system".to_string())
        );
    }

    #[test]
    fn cluster_wide_scope_describes_itself_for_logs() {
        assert_eq!(WatchScope::AllNamespaces.describe(), "all namespaces");
    }
}
