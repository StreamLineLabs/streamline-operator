//! Streamline Kubernetes Operator
//!
//! A Kubernetes operator for deploying and managing Streamline clusters.
//!
//! ## Custom Resources
//!
//! - `StreamlineCluster`: Deploys a Streamline cluster as a StatefulSet
//! - `StreamlineTopic`: Creates and manages topics within a cluster
//! - `StreamlineUser`: Creates and manages users with authentication/authorization
//!
//! ## Example
//!
//! ```yaml
//! apiVersion: streamline.io/v1alpha1
//! kind: StreamlineCluster
//! metadata:
//!   name: my-cluster
//! spec:
//!   replicas: 3
//!   storage:
//!     size: 10Gi
//! ```

pub mod conditions;
pub mod controllers;
pub mod crd;
pub mod error;
pub mod leader_election;

pub use controllers::{ClusterController, TopicController, UserController};
pub use crd::{
    ClusterCondition, ClusterPhase, ClusterSpec, ClusterStatus, ClusterStorage, ClusterTls,
    ResourceRequirements, StreamlineCluster, StreamlineTopic, StreamlineUser, TopicSpec,
    TopicStatus, UserCredentials, UserPermission, UserSpec, UserStatus,
};
pub use error::{OperatorError, Result};
