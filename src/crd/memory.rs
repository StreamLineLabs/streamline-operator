//! StreamlineMemory CRD for agent memory coordination (M1 P4).
//!
//! Declarative memory topology for AI agent memory tiers. A `StreamlineMemory`
//! resource manages episodic, semantic, and procedural memory topics for a
//! specific agent, including retention policies, decay configuration, and
//! optional encryption.

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// StreamlineMemory is the Schema for the streamlinememories API.
#[derive(CustomResource, Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "streamline.io",
    version = "v1alpha1",
    kind = "StreamlineMemory",
    namespaced,
    status = "MemoryStatus",
    shortname = "slm",
    printcolumn = r#"{"name":"Agent","type":"string","jsonPath":".spec.agentId"}"#,
    printcolumn = r#"{"name":"Tenant","type":"string","jsonPath":".spec.tenant"}"#,
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct MemorySpec {
    /// Unique agent identifier that owns this memory partition.
    pub agent_id: String,

    /// Tenant or namespace for multi-tenant deployments.
    pub tenant: String,

    /// Reference to the parent StreamlineCluster (by metadata.name).
    pub cluster_ref: String,

    /// Memory tier retention configuration.
    #[serde(default)]
    pub tiers: MemoryTiers,

    /// Memory decay configuration for automatic relevance scoring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decay: Option<MemoryDecay>,

    /// Whether to enable encryption at rest for memory topics.
    #[serde(default)]
    pub encryption_enabled: bool,
}

/// Retention configuration for each memory tier.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MemoryTiers {
    /// Retention in days for episodic (event-based) memories. Default: 30.
    #[serde(default = "default_episodic_retention")]
    pub episodic_retention_days: i64,

    /// Retention in days for semantic (knowledge) memories. Default: 365.
    #[serde(default = "default_semantic_retention")]
    pub semantic_retention_days: i64,

    /// Retention in days for procedural (skill/habit) memories. Default: -1 (infinite).
    #[serde(default = "default_procedural_retention")]
    pub procedural_retention_days: i64,
}

impl Default for MemoryTiers {
    fn default() -> Self {
        Self {
            episodic_retention_days: default_episodic_retention(),
            semantic_retention_days: default_semantic_retention(),
            procedural_retention_days: default_procedural_retention(),
        }
    }
}

fn default_episodic_retention() -> i64 {
    30
}
fn default_semantic_retention() -> i64 {
    365
}
fn default_procedural_retention() -> i64 {
    -1
}

/// Decay configuration for automatic memory relevance scoring.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MemoryDecay {
    /// Half-life in days for memory decay scoring.
    #[serde(default = "default_half_life")]
    pub half_life_days: f64,

    /// Threshold below which memories are eligible for garbage collection.
    #[serde(default = "default_threshold")]
    pub threshold: f64,
}

fn default_half_life() -> f64 {
    7.0
}
fn default_threshold() -> f64 {
    0.1
}

impl Default for MemoryDecay {
    fn default() -> Self {
        Self {
            half_life_days: default_half_life(),
            threshold: default_threshold(),
        }
    }
}

/// Lifecycle phase reported by the controller.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default)]
pub enum MemoryPhase {
    #[default]
    Pending,
    Provisioning,
    Ready,
    Decaying,
    Failed,
    Deleting,
}

/// MemoryStatus describes the observed state of an agent's memory.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct MemoryStatus {
    /// Current lifecycle phase.
    #[serde(default)]
    pub phase: MemoryPhase,

    /// Whether the memory topics are ready.
    #[serde(default)]
    pub ready: bool,

    /// Number of episodic memory events stored.
    #[serde(default)]
    pub episodic_event_count: i64,

    /// Number of semantic memory entries stored.
    #[serde(default)]
    pub semantic_event_count: i64,

    /// Number of procedural memory entries stored.
    #[serde(default)]
    pub procedural_event_count: i64,

    /// Last observed message from the reconciler.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,

    /// Standard k8s-style conditions.
    #[serde(default)]
    pub conditions: Vec<MemoryCondition>,
}

/// A single status condition.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemoryCondition {
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
    fn test_memory_spec_minimal() {
        let json = r#"{"agentId":"agent-001","tenant":"default","clusterRef":"prod"}"#;
        let spec: MemorySpec = serde_json::from_str(json).unwrap();
        assert_eq!(spec.agent_id, "agent-001");
        assert_eq!(spec.tenant, "default");
        assert_eq!(spec.cluster_ref, "prod");
        assert_eq!(spec.tiers.episodic_retention_days, 30);
        assert_eq!(spec.tiers.semantic_retention_days, 365);
        assert_eq!(spec.tiers.procedural_retention_days, -1);
        assert!(spec.decay.is_none());
        assert!(!spec.encryption_enabled);
    }

    #[test]
    fn test_memory_spec_full() {
        let json = r#"{
            "agentId": "agent-002",
            "tenant": "acme",
            "clusterRef": "staging",
            "tiers": {
                "episodicRetentionDays": 7,
                "semanticRetentionDays": 90,
                "proceduralRetentionDays": 180
            },
            "decay": {
                "halfLifeDays": 14.0,
                "threshold": 0.05
            },
            "encryptionEnabled": true
        }"#;
        let spec: MemorySpec = serde_json::from_str(json).unwrap();
        assert_eq!(spec.tiers.episodic_retention_days, 7);
        assert_eq!(spec.tiers.semantic_retention_days, 90);
        assert_eq!(spec.tiers.procedural_retention_days, 180);
        let decay = spec.decay.unwrap();
        assert!((decay.half_life_days - 14.0).abs() < f64::EPSILON);
        assert!((decay.threshold - 0.05).abs() < f64::EPSILON);
        assert!(spec.encryption_enabled);
    }

    #[test]
    fn test_memory_phase_default() {
        assert_eq!(MemoryPhase::default(), MemoryPhase::Pending);
    }

    #[test]
    fn test_memory_status_round_trip() {
        let s = MemoryStatus {
            phase: MemoryPhase::Ready,
            ready: true,
            episodic_event_count: 42,
            semantic_event_count: 10,
            procedural_event_count: 3,
            message: Some("ok".into()),
            conditions: vec![MemoryCondition {
                r#type: "Ready".into(),
                status: "True".into(),
                last_transition_time: None,
                reason: None,
                message: None,
            }],
        };
        let buf = serde_json::to_string(&s).unwrap();
        let back: MemoryStatus = serde_json::from_str(&buf).unwrap();
        assert_eq!(back.phase, MemoryPhase::Ready);
        assert!(back.ready);
        assert_eq!(back.episodic_event_count, 42);
        assert_eq!(back.conditions.len(), 1);
    }

    #[test]
    fn test_memory_crd_renders() {
        let crd = StreamlineMemory::crd();
        assert_eq!(crd.spec.group, "streamline.io");
        assert_eq!(crd.spec.names.kind, "StreamlineMemory");
    }
}
