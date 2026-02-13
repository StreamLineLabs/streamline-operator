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

    /// Replication factor for the topic
    #[serde(default = "default_replication_factor")]
    pub replication_factor: i32,

    /// Topic configuration overrides
    #[serde(default)]
    pub config: TopicConfig,

    /// Retention configuration
    #[serde(default)]
    pub retention: RetentionConfig,

    /// Compression configuration
    #[serde(default)]
    pub compression: CompressionConfig,
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
    /// Retention time in milliseconds (-1 for unlimited)
    #[serde(default = "default_retention_ms")]
    pub retention_ms: i64,

    /// Maximum size in bytes per partition (-1 for unlimited)
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
    2
}

fn default_retention_ms() -> i64 {
    604_800_000 // 7 days
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
    use super::*;

    #[test]
    fn test_topic_spec_defaults() {
        let json = r#"{"clusterRef": "my-cluster"}"#;
        let spec: TopicSpec = serde_json::from_str(json).unwrap();
        assert_eq!(spec.partitions, 3);
        assert_eq!(spec.replication_factor, 2);
        assert_eq!(spec.cluster_ref, "my-cluster");
    }

    #[test]
    fn test_retention_defaults() {
        let retention = RetentionConfig::default();
        assert_eq!(retention.retention_ms, 604_800_000);
        assert_eq!(retention.retention_bytes, -1);
        assert_eq!(retention.cleanup_policy, "delete");
    }

    #[test]
    fn test_topic_phase_default() {
        let phase = TopicPhase::default();
        assert_eq!(phase, TopicPhase::Pending);
    }
}
