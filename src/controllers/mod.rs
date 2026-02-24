//! Controllers for Streamline Kubernetes Operator
//!
//! Each controller watches its respective CRD and reconciles the actual state
//! with the desired state specified in the custom resources.

mod autoscaling;
mod cluster;
mod topic;
mod user;

pub use autoscaling::{
    AutoScalingConfig, AutoScalingController, CustomMetric, MetricTargetSpec, PartitionMetrics,
    ScalingBehavior, ScalingPolicy, ScalingRecommendation, ScalingRules,
};
pub use cluster::ClusterController;
pub use topic::TopicController;
pub use user::UserController;

use crate::error::OperatorError;
use kube::runtime::controller::Action;
use std::time::Duration;

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
