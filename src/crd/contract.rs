//! StreamlineContract Custom Resource Definition
//!
//! Declarative data contracts (Moonshot M4). A `StreamlineContract` registers
//! an enforced schema with the broker; producers writing to topics bound to
//! the contract must satisfy it. As with `StreamlineBranch`, this scaffold
//! intentionally omits a live controller — the reconcile loop against the
//! Moonshot HTTP control plane will land in a follow-up.

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// StreamlineContract is the Schema for the streamlinecontracts API.
#[derive(CustomResource, Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "streamline.io",
    version = "v1alpha1",
    kind = "StreamlineContract",
    namespaced,
    status = "ContractStatus",
    shortname = "slc",
    printcolumn = r#"{"name":"Cluster","type":"string","jsonPath":".spec.clusterRef"}"#,
    printcolumn = r#"{"name":"Compatibility","type":"string","jsonPath":".spec.compatibility"}"#,
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct ContractSpec {
    /// Reference to the parent StreamlineCluster.
    pub cluster_ref: String,

    /// JSON-encoded schema body. Stored verbatim; the broker validates it on
    /// register. Kept as a string so the CRD schema does not have to anticipate
    /// every Moonshot schema dialect.
    pub schema_json: String,

    /// Compatibility policy. Defaults to `BACKWARD` server-side when empty.
    #[serde(default = "default_compatibility")]
    pub compatibility: ContractCompatibility,

    /// Optional list of topic name patterns this contract should bind to.
    #[serde(default)]
    pub bind_topics: Vec<String>,
}

/// Schema compatibility policies recognised by the Moonshot control plane.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum ContractCompatibility {
    Backward,
    Forward,
    Full,
    None,
}

impl Default for ContractCompatibility {
    fn default() -> Self {
        Self::Backward
    }
}

fn default_compatibility() -> ContractCompatibility {
    ContractCompatibility::Backward
}

/// Lifecycle phase reported by the controller.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default)]
pub enum ContractPhase {
    #[default]
    Pending,
    Registering,
    Active,
    Failed,
    Deleting,
}

/// ContractStatus describes the observed state of a contract.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ContractStatus {
    #[serde(default)]
    pub phase: ContractPhase,

    /// Whether the contract is currently registered and active.
    #[serde(default)]
    pub registered: bool,

    /// Topics the broker confirms are bound to this contract.
    #[serde(default)]
    pub bound_topics: Vec<String>,

    /// Last observed message from the broker / reconciler.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,

    /// Standard k8s-style conditions.
    #[serde(default)]
    pub conditions: Vec<ContractCondition>,
}

/// A single status condition.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContractCondition {
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
    fn test_contract_spec_minimal() {
        let json = r#"{"clusterRef":"prod","schemaJson":"{\"type\":\"object\"}"}"#;
        let spec: ContractSpec = serde_json::from_str(json).unwrap();
        assert_eq!(spec.cluster_ref, "prod");
        assert!(spec.schema_json.contains("object"));
        assert_eq!(spec.compatibility, ContractCompatibility::Backward);
        assert!(spec.bind_topics.is_empty());
    }

    #[test]
    fn test_contract_compatibility_serialization() {
        let s = serde_json::to_string(&ContractCompatibility::Full).unwrap();
        assert_eq!(s, "\"FULL\"");
        let back: ContractCompatibility = serde_json::from_str("\"NONE\"").unwrap();
        assert_eq!(back, ContractCompatibility::None);
    }

    #[test]
    fn test_contract_spec_with_bindings() {
        let json = r#"{"clusterRef":"prod","schemaJson":"{}","compatibility":"FORWARD","bindTopics":["orders","payments"]}"#;
        let spec: ContractSpec = serde_json::from_str(json).unwrap();
        assert_eq!(spec.compatibility, ContractCompatibility::Forward);
        assert_eq!(spec.bind_topics, vec!["orders", "payments"]);
    }

    #[test]
    fn test_contract_phase_default() {
        assert_eq!(ContractPhase::default(), ContractPhase::Pending);
    }

    #[test]
    fn test_contract_crd_renders() {
        let crd = StreamlineContract::crd();
        assert_eq!(crd.spec.group, "streamline.io");
        assert_eq!(crd.spec.names.kind, "StreamlineContract");
    }
}
