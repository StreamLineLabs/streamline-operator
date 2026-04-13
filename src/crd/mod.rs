//! Custom Resource Definitions for Streamline Kubernetes Operator
//!
//! Defines the CRDs that the operator manages:
//! - StreamlineCluster: A Streamline cluster deployment
//! - StreamlineTopic: A topic within a cluster
//! - StreamlineUser: A user with authentication credentials
//! - StreamlineBackup: A backup schedule and retention policy
//! - StreamlineBranch: A time-travel branch (Moonshot M5; controller TBD)
//! - StreamlineContract: An enforced data contract (Moonshot M4; controller TBD)

mod backup;
mod branch;
mod cluster;
mod contract;
mod edge;
mod memory;
mod topic;
mod user;

pub use backup::{
    BackupCondition, BackupPhase, BackupSpec, BackupStatus, BackupStorage, BackupStorageType,
    BackupType, StreamlineBackup,
};
pub use branch::{
    BranchCondition, BranchPhase, BranchRetention, BranchSpec, BranchStatus, StreamlineBranch,
};
pub use cluster::{
    AutoScalingSpec, ClusterCondition, ClusterPhase, ClusterSpec, ClusterStatus, ClusterStorage,
    ClusterTls, ResourceRequirements, ScalingBehaviorSpec, ScalingPolicySpec, ScalingRulesSpec,
    StreamlineCluster,
};
pub use contract::{
    ContractCompatibility, ContractCondition, ContractPhase, ContractSpec, ContractStatus,
    StreamlineContract,
};
pub use edge::{
    BootstrapConfig, EdgeCondition, EdgeSpec, EdgeStatus, StreamlineEdge, SyncConfig,
};
pub use memory::{
    MemoryCondition, MemoryDecay, MemoryPhase, MemorySpec, MemoryStatus, MemoryTiers,
    StreamlineMemory,
};
pub use topic::{StreamlineTopic, TopicCondition, TopicPhase, TopicSpec, TopicStatus};
pub use user::{
    StreamlineUser, UserCondition, UserCredentials, UserPermission, UserPhase, UserSpec, UserStatus,
};
