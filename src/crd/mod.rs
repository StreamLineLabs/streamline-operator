//! Custom Resource Definitions for Streamline Kubernetes Operator
//!
//! Defines the CRDs that the operator manages:
//! - StreamlineCluster: A Streamline cluster deployment
//! - StreamlineTopic: A topic within a cluster
//! - StreamlineUser: A user with authentication credentials

mod cluster;
mod topic;
mod user;

pub use cluster::{
    AutoScalingSpec, ClusterCondition, ClusterPhase, ClusterSpec, ClusterStatus, ClusterStorage,
    ClusterTls, ResourceRequirements, ScalingBehaviorSpec, ScalingPolicySpec, ScalingRulesSpec,
    StreamlineCluster,
};
pub use topic::{StreamlineTopic, TopicCondition, TopicPhase, TopicSpec, TopicStatus};
pub use user::{
    StreamlineUser, UserCondition, UserCredentials, UserPermission, UserPhase, UserSpec, UserStatus,
};
