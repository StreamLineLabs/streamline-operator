//! StreamlineTopic Custom Resource Definition
//!
//! Defines the specification for creating and managing topics within a Streamline cluster.

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// StreamlineTopic is the Schema for the streamlinetopics API
#[derive(CustomResource, Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "streamline.io",
    version = "v1alpha1",
    kind = "StreamlineTopic",
    namespaced,
    status = "TopicStatus",
    shortname = "slt",
    printcolumn = r#"{"name":"Cluster","type":"string","jsonPath":".spec.clusterRef"}"#,
    printcolumn = r#"{"name":"Partitions","type":"integer","jsonPath":".spec.partitions"}"#,
    printcolumn = r#"{"name":"Replication","type":"integer","jsonPath":".spec.replicationFactor"}"#,
    printcolumn = r#"{"name":"Ready","type":"boolean","jsonPath":".status.ready"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct TopicSpec {
    /// Reference to the StreamlineCluster
    pub cluster_ref: String,

    /// Number of partitions for the topic
    #[serde(default = "default_partitions")]
    pub partitions: i32,

    /// Replication factor for the topic.
    ///
    /// Defaults to 1: the Streamline topic API creates single-replica topics,
    /// and `TopicController` rejects any other value rather than reporting
    /// `Ready` for durability the cluster does not provide.
    #[serde(default = "default_replication_factor")]
    pub replication_factor: i32,

    /// Topic configuration overrides.
    ///
    /// Not applied: the Streamline topic API accepts only a name and a
    /// partition count, so every override here is rejected by
    /// `TopicController` instead of being silently discarded. Leave unset.
    #[serde(default)]
    pub config: TopicConfig,

    /// Retention configuration.
    ///
    /// Not applied: the Streamline topic API exposes no way to set retention
    /// on a topic, so any value other than the defaults below is rejected
    /// rather than silently discarded. The defaults describe what the broker
    /// actually does — it retains every topic indefinitely — so leaving them
    /// alone is an accurate description of the result.
    #[serde(default)]
    pub retention: RetentionConfig,

    /// Compression configuration.
    ///
    /// Not applied: compression is chosen by the producer, and the topic API
    /// accepts no compression setting, so any value other than the default
    /// (`producer`) is rejected rather than silently discarded.
    #[serde(default)]
    pub compression: CompressionConfig,
}

/// The schema defaults for the topic settings the server cannot apply.
///
/// `TopicController::unsupported_fields` rejects anything that differs from
/// these, so keeping the comparison basis next to the defaults themselves
/// stops the two drifting apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicConfigDefaults {
    /// Default `spec.retention.retentionMs`.
    pub retention_ms: i64,
    /// Default `spec.retention.retentionBytes`.
    pub retention_bytes: i64,
    /// Default `spec.retention.cleanupPolicy`.
    pub cleanup_policy: String,
    /// Default `spec.compression.type`.
    pub compression_type: String,
}

impl TopicSpec {
    /// Schema defaults for the settings the Streamline server does not apply.
    pub fn config_defaults() -> TopicConfigDefaults {
        let retention = RetentionConfig::default();
        TopicConfigDefaults {
            retention_ms: retention.retention_ms,
            retention_bytes: retention.retention_bytes,
            cleanup_policy: retention.cleanup_policy,
            compression_type: CompressionConfig::default().r#type,
        }
    }
}

/// Topic configuration overrides
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct TopicConfig {
    /// Minimum in-sync replicas required for writes
    #[serde(default)]
    pub min_insync_replicas: Option<i32>,

    /// Maximum message size in bytes
    #[serde(default)]
    pub max_message_bytes: Option<i64>,

    /// Segment size in bytes
    #[serde(default)]
    pub segment_bytes: Option<i64>,

    /// Index interval in bytes
    #[serde(default)]
    pub index_interval_bytes: Option<i64>,

    /// Flush interval in milliseconds
    #[serde(default)]
    pub flush_interval_ms: Option<i64>,

    /// Flush after N messages
    #[serde(default)]
    pub flush_messages: Option<i64>,

    /// Additional custom configurations
    #[serde(default)]
    pub custom: std::collections::BTreeMap<String, String>,
}

/// Retention configuration
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RetentionConfig {
    /// Retention time in milliseconds; `-1` (the default) means unlimited.
    ///
    /// Unlimited is the default because the core broker applies no topic
    /// configuration and never expires a segment on age. A `604800000`
    /// default described a seven-day policy nothing implemented, and since
    /// every non-default value is rejected, that made the *only* accepted
    /// value the one that was wrong.
    #[serde(default = "default_retention_ms")]
    pub retention_ms: i64,

    /// Maximum size in bytes per partition; `-1` (the default) means
    /// unlimited, which is what the broker does.
    #[serde(default = "default_retention_bytes")]
    pub retention_bytes: i64,

    /// Delete or compact cleanup policy
    #[serde(default = "default_cleanup_policy")]
    pub cleanup_policy: String,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            retention_ms: default_retention_ms(),
            retention_bytes: default_retention_bytes(),
            cleanup_policy: default_cleanup_policy(),
        }
    }
}

/// Compression configuration
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CompressionConfig {
    /// Compression type (none, gzip, snappy, lz4, zstd)
    #[serde(default = "default_compression_type")]
    pub r#type: String,
}

impl Default for CompressionConfig {
    fn default() -> Self {
        Self {
            r#type: default_compression_type(),
        }
    }
}

/// Status of the StreamlineTopic
// `PartialEq` is derived so the controller can compare the status it wants to
// publish against the one already on the object and skip the patch when
// nothing changed; patching an identical status re-triggers the watch and
// spins the reconcile loop. Kept as a plain comment: doc comments become the
// user-facing `description` in the generated CRD schema.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TopicStatus {
    /// Whether the topic is ready
    #[serde(default)]
    pub ready: bool,

    /// Current phase of the topic
    #[serde(default)]
    pub phase: TopicPhase,

    /// Actual number of partitions
    #[serde(default)]
    pub partitions: i32,

    /// Actual replication factor
    #[serde(default)]
    pub replication_factor: i32,

    /// Partition assignment information
    #[serde(default)]
    pub partition_assignments: Vec<PartitionAssignment>,

    /// Conditions representing topic state
    #[serde(default)]
    pub conditions: Vec<TopicCondition>,

    /// Last observed generation
    #[serde(default)]
    pub observed_generation: Option<i64>,

    /// Last update timestamp
    #[serde(default)]
    pub last_updated: Option<String>,

    /// Error message if creation failed
    #[serde(default)]
    pub error_message: Option<String>,
}

/// Phase of the topic lifecycle
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default, PartialEq, Eq)]
pub enum TopicPhase {
    /// Topic is being created
    #[default]
    Pending,
    /// Topic is ready
    Ready,
    /// Topic is being updated
    Updating,
    /// Topic creation/update failed
    Failed,
    /// Topic is being deleted
    Terminating,
}

/// Partition assignment information
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct PartitionAssignment {
    /// Partition ID
    pub partition: i32,
    /// Leader broker ID
    pub leader: i32,
    /// Replica broker IDs
    pub replicas: Vec<i32>,
    /// In-sync replica broker IDs
    pub isr: Vec<i32>,
}

/// Condition of the topic
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TopicCondition {
    /// Type of condition
    pub r#type: String,
    /// Status of the condition (True, False, Unknown)
    pub status: String,
    /// Last time the condition transitioned
    #[serde(default)]
    pub last_transition_time: Option<String>,
    /// Reason for the condition
    #[serde(default)]
    pub reason: Option<String>,
    /// Human-readable message
    #[serde(default)]
    pub message: Option<String>,
}

// Default value functions
fn default_partitions() -> i32 {
    3
}

fn default_replication_factor() -> i32 {
    1
}

/// Unlimited retention, matching [`default_retention_bytes`] and the broker's
/// actual behaviour.
///
/// This was `604_800_000` (7 days). The core topic API accepts only
/// `{name, partitions}` and applies no topic configuration, so nothing ever
/// enforced that window: the schema default advertised a retention policy the
/// broker does not have. Because `TopicController::unsupported_fields` rejects
/// every *non-default* value, users could not correct it either — the default
/// was simultaneously the only permitted value and a false claim. `-1` states
/// what actually happens: topics are retained indefinitely.
fn default_retention_ms() -> i64 {
    -1 // unlimited: core applies no topic retention
}

fn default_retention_bytes() -> i64 {
    -1 // unlimited
}

fn default_cleanup_policy() -> String {
    "delete".to_string()
}

fn default_compression_type() -> String {
    "producer".to_string()
}

#[cfg(test)]
mod tests {
    // unwrap/expect are acceptable in tests; the crate-wide lint targets production code.
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn test_topic_spec_defaults() {
        let json = r#"{"clusterRef": "my-cluster"}"#;
        let spec: TopicSpec = serde_json::from_str(json).unwrap();
        assert_eq!(spec.partitions, 3);
        assert_eq!(spec.replication_factor, 1);
        assert_eq!(spec.cluster_ref, "my-cluster");
    }

    /// The schema defaults must describe what the broker does, not a policy it
    /// does not implement. Core applies no topic configuration, so retention
    /// is unlimited on both axes.
    #[test]
    fn test_retention_defaults() {
        let retention = RetentionConfig::default();
        assert_eq!(
            retention.retention_ms, -1,
            "retentionMs must default to unlimited: core never expires a segment on age"
        );
        assert_eq!(retention.retention_bytes, -1);
        assert_eq!(retention.cleanup_policy, "delete");
    }

    /// A deserialised spec that omits `retention` must land on the same
    /// unlimited defaults, so the value the controller compares against (and
    /// the value the API server injects) can never be the old seven-day claim.
    #[test]
    fn an_unspecified_retention_is_unlimited_not_seven_days() {
        let spec: TopicSpec = serde_json::from_str(r#"{"clusterRef": "my-cluster"}"#).unwrap();

        assert_eq!(spec.retention.retention_ms, -1);
        assert_ne!(
            spec.retention.retention_ms, 604_800_000,
            "the 7-day default described retention the server never applies"
        );
        assert_eq!(
            spec.retention.retention_ms, spec.retention.retention_bytes,
            "both retention axes are unlimited; they must not disagree"
        );
        assert_eq!(TopicSpec::config_defaults().retention_ms, -1);
    }

    #[test]
    fn config_defaults_match_the_schema_defaults() {
        let defaults = TopicSpec::config_defaults();
        let spec: TopicSpec = serde_json::from_str(r#"{"clusterRef": "c"}"#).unwrap();

        assert_eq!(defaults.retention_ms, spec.retention.retention_ms);
        assert_eq!(defaults.retention_bytes, spec.retention.retention_bytes);
        assert_eq!(defaults.cleanup_policy, spec.retention.cleanup_policy);
        assert_eq!(defaults.compression_type, spec.compression.r#type);
        assert!(spec.config.min_insync_replicas.is_none());
        assert!(spec.config.custom.is_empty());
    }

    #[test]
    fn test_topic_phase_default() {
        let phase = TopicPhase::default();
        assert_eq!(phase, TopicPhase::Pending);
    }
}
