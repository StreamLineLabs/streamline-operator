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
pub mod metrics;

pub use controllers::{
    BranchController, ClusterController, ContractController, MemoryController, TopicController,
    UserController,
};
pub use crd::{
    BackupCondition, BackupPhase, BackupSpec, BackupStatus, BackupStorage, BackupType,
    BranchPhase, BranchSpec, BranchStatus, ClusterCondition, ClusterPhase, ClusterSpec,
    ClusterStatus, ClusterStorage, ClusterTls, ContractCompatibility, ContractPhase,
    ContractSpec, ContractStatus, MemoryCondition, MemoryDecay, MemoryPhase, MemorySpec,
    MemoryStatus, MemoryTiers, ResourceRequirements, StreamlineBackup, StreamlineBranch,
    StreamlineCluster, StreamlineContract, StreamlineMemory, StreamlineTopic, StreamlineUser,
    TopicSpec, TopicStatus, UserCredentials, UserPermission, UserSpec, UserStatus,
};
pub use error::{OperatorError, Result};

