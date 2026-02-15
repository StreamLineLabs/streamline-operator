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

/// Common trait for all controllers
#[async_trait::async_trait]
pub trait Controller: Send + Sync {
    /// Start the controller's reconciliation loop
    async fn run(&self) -> Result<(), OperatorError>;

    /// Get the controller name for logging
    fn name(&self) -> &'static str;
}
