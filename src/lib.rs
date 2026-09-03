//! Streamline Kubernetes Operator
//!
//! A Kubernetes operator for deploying and managing Streamline clusters.
//!
//! ## Installed custom resources
//!
//! - `StreamlineCluster`: deploys a Streamline cluster as a StatefulSet.
//!   Each replica is a standalone broker, so `spec.replicas` must be `1` and
//!   `spec.autoscaling.enabled: true` is rejected.
//! - `StreamlineTopic`: creates topics through the Streamline HTTP API, which
//!   accepts only a name and a partition count. `replicationFactor` must be
//!   `1`, and retention/compression/cleanup/`config` overrides are rejected
//!   rather than silently discarded.
//! - `StreamlineUser`: **unsupported**. The server has no user API, so the
//!   controller reports `Unsupported` and creates nothing.
//!
//! ## Upgrading from v0.3.0
//!
//! The v0.3.0 CRDs persisted `spec.replicas: 3`, `spec.podAntiAffinity: true`,
//! and `spec.replicationFactor: 2` into stored objects. This operator rejects
//! all three, so resources created against those CRDs must be patched before
//! it is deployed. See [`upgrade`] and `docs/UPGRADING.md`.
//!
//! ## Schema-only custom resources
//!
//! `StreamlineBranch`, `StreamlineContract`, `StreamlineMemory`,
//! `StreamlineBackup`, and `StreamlineEdge` types exist so the schemas can be
//! generated and reviewed, but no controller reconciles them and
//! `deploy/crds/kustomization.yaml` does not install them. See
//! [`crd::generate::Reconciliation`] for the per-kind reason.
//!
//! ## Example
//!
//! ```yaml
//! apiVersion: streamline.io/v1alpha1
//! kind: StreamlineCluster
//! metadata:
//!   name: my-cluster
//! spec:
//!   replicas: 1
//!   storage:
//!     size: 10Gi
//! ```
//!
//! ```yaml
//! apiVersion: streamline.io/v1alpha1
//! kind: StreamlineTopic
//! metadata:
//!   name: events
//!   namespace: streamline-system
//! spec:
//!   clusterRef: my-cluster
//!   partitions: 6
//!   replicationFactor: 1
//! ```
//!
//! Both examples are checked against the generated CRD schemas by
//! `tests/docs_examples.rs`.

pub mod conditions;
pub mod controllers;
pub mod crd;
pub mod error;
pub mod leader_election;
pub mod metrics;
pub mod upgrade;

pub use controllers::{ClusterController, TopicController, UserController};
// Schema types for every CRD this crate defines, including the schema-only
// kinds. Exporting the type is not a claim that a controller reconciles it —
// `crd::generate::Reconciliation` is the source of truth for that.
pub use crd::{
    BackupCondition, BackupPhase, BackupSpec, BackupStatus, BackupStorage, BackupType, BranchPhase,
    BranchSpec, BranchStatus, ClusterCondition, ClusterPhase, ClusterSpec, ClusterStatus,
    ClusterStorage, ClusterTls, ContractCompatibility, ContractPhase, ContractSpec, ContractStatus,
    MemoryCondition, MemoryDecay, MemoryPhase, MemorySpec, MemoryStatus, MemoryTiers,
    ResourceRequirements, StreamlineBackup, StreamlineBranch, StreamlineCluster,
    StreamlineContract, StreamlineMemory, StreamlineTopic, StreamlineUser, TopicConfigDefaults,
    TopicSpec, TopicStatus, UserCredentials, UserPermission, UserSpec, UserStatus,
};
pub use error::{OperatorError, Result};
