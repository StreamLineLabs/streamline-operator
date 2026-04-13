//! StreamlineBranch Custom Resource Definition
//!
//! Declarative time-travel branches (Moonshot M5). A `StreamlineBranch`
//! references a parent cluster + optional source branch and exposes the
//! branch's lifecycle through `status`. Controllers are intentionally NOT
//! implemented here: this CRD scaffold lets users declare branches in GitOps
//! flows, and a subsequent change will wire the reconcile loop against the
//! Moonshot HTTP control plane.

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// StreamlineBranch is the Schema for the streamlinebranches API.
#[derive(CustomResource, Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "streamline.io",
    version = "v1alpha1",
    kind = "StreamlineBranch",
    namespaced,
    status = "BranchStatus",
    shortname = "slb",
    printcolumn = r#"{"name":"Cluster","type":"string","jsonPath":".spec.clusterRef"}"#,
    printcolumn = r#"{"name":"Parent","type":"string","jsonPath":".spec.parent"}"#,
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct BranchSpec {
    /// Reference to the parent StreamlineCluster (by metadata.name).
    pub cluster_ref: String,

    /// Parent branch name. Defaults to the cluster's main branch when empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,

    /// Optional human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Optional retention policy for branch state. When `None`, the cluster
    /// default applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<BranchRetention>,
}

/// Branch retention policy.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BranchRetention {
    /// TTL in seconds after which the branch may be GC'd. 0 = keep forever.
    #[serde(default)]
    pub ttl_seconds: i64,
}

/// Lifecycle phase reported by the controller.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default)]
pub enum BranchPhase {
    #[default]
    Pending,
    Creating,
    Ready,
    Failed,
    Deleting,
}

/// BranchStatus describes the observed state of a branch.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct BranchStatus {
    /// Current lifecycle phase.
    #[serde(default)]
    pub phase: BranchPhase,

    /// Whether the branch is ready for reads.
    #[serde(default)]
    pub ready: bool,

    /// Branch creation timestamp (Unix epoch milliseconds), as reported by
    /// the broker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at_ms: Option<i64>,

    /// Last observed message from the broker / reconciler.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,

    /// Standard k8s-style conditions.
    #[serde(default)]
    pub conditions: Vec<BranchCondition>,
}

/// A single status condition.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BranchCondition {
    pub r#type: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_transition_time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use kube::CustomResourceExt;

    #[test]
    fn test_branch_spec_minimal() {
        let json = r#"{"clusterRef":"prod"}"#;
        let spec: BranchSpec = serde_json::from_str(json).unwrap();
        assert_eq!(spec.cluster_ref, "prod");
        assert!(spec.parent.is_none());
        assert!(spec.retention.is_none());
    }

    #[test]
    fn test_branch_spec_with_parent_and_retention() {
        let json = r#"{"clusterRef":"prod","parent":"main","retention":{"ttlSeconds":86400}}"#;
        let spec: BranchSpec = serde_json::from_str(json).unwrap();
        assert_eq!(spec.parent.as_deref(), Some("main"));
        assert_eq!(spec.retention.unwrap().ttl_seconds, 86400);
    }

    #[test]
    fn test_branch_phase_default_pending() {
        assert_eq!(BranchPhase::default(), BranchPhase::Pending);
    }

    #[test]
    fn test_branch_status_round_trip() {
        let s = BranchStatus {
            phase: BranchPhase::Ready,
            ready: true,
            created_at_ms: Some(99),
            message: Some("ok".into()),
            conditions: vec![BranchCondition {
                r#type: "Ready".into(),
                status: "True".into(),
                last_transition_time: None,
                reason: None,
                message: None,
            }],
        };
        let buf = serde_json::to_string(&s).unwrap();
        let back: BranchStatus = serde_json::from_str(&buf).unwrap();
        assert_eq!(back.phase, BranchPhase::Ready);
        assert!(back.ready);
        assert_eq!(back.created_at_ms, Some(99));
        assert_eq!(back.conditions.len(), 1);
    }

    #[test]
    fn test_branch_crd_renders() {
        let crd = StreamlineBranch::crd();
        assert_eq!(crd.spec.group, "streamline.io");
        assert_eq!(crd.spec.names.kind, "StreamlineBranch");
    }
}
