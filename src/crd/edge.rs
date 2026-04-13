//! StreamlineEdge Custom Resource Definition
//!
//! Declarative edge node fleet management (Moonshot M3). A `StreamlineEdge`
//! resource manages a fleet of edge nodes that sync with the cluster.

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// StreamlineEdge is the Schema for the streamlineedges API.
#[derive(CustomResource, Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "streamline.io",
    version = "v1alpha1",
    kind = "StreamlineEdge",
    namespaced,
    status = "EdgeStatus",
    shortname = "sle",
    printcolumn = r#"{"name":"Cluster","type":"string","jsonPath":".spec.clusterRef"}"#,
    printcolumn = r#"{"name":"Nodes","type":"integer","jsonPath":".status.connectedNodes"}"#,
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct EdgeSpec {
    /// Reference to the parent StreamlineCluster (by metadata.name).
    pub cluster_ref: String,

    /// Topics to sync with edge nodes.
    #[serde(default)]
    pub sync_topics: Vec<String>,

    /// CRDT type for synced topics (lww, gset, or-set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_crdt_type: Option<String>,

    /// Maximum number of edge nodes allowed.
    #[serde(default = "default_max_nodes")]
    pub max_nodes: u32,

    /// Bootstrap token configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootstrap: Option<BootstrapConfig>,

    /// Sync configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync: Option<SyncConfig>,
}

/// Bootstrap token configuration for edge nodes.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapConfig {
    /// Secret name containing the bootstrap token.
    pub token_secret_ref: String,
    /// Token TTL in seconds (0 = no expiry).
    #[serde(default)]
    pub ttl_seconds: u64,
    /// Whether tokens are single-use.
    #[serde(default)]
    pub single_use: bool,
}

/// Sync protocol configuration.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SyncConfig {
    /// Sync batch interval in milliseconds.
    #[serde(default = "default_batch_interval")]
    pub batch_interval_ms: u64,
    /// Maximum batch size in bytes.
    #[serde(default = "default_max_batch_bytes")]
    pub max_batch_bytes: u64,
    /// Enable bandwidth-aware adaptive sync.
    #[serde(default)]
    pub adaptive_sync: bool,
}

fn default_max_nodes() -> u32 {
    100
}
fn default_batch_interval() -> u64 {
    1000
}
fn default_max_batch_bytes() -> u64 {
    1_048_576
}

/// EdgeStatus defines the observed state of StreamlineEdge.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EdgeStatus {
    /// Current phase: Provisioning, Active, Degraded, Terminated.
    #[serde(default)]
    pub phase: String,

    /// Number of currently connected edge nodes.
    #[serde(default)]
    pub connected_nodes: u32,

    /// Total number of registered edge nodes (including offline).
    #[serde(default)]
    pub registered_nodes: u32,

    /// Last time any node synced successfully.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sync_at: Option<String>,

    /// Status conditions (standard Kubernetes pattern).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<EdgeCondition>,
}

/// Individual status condition for edge fleet.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EdgeCondition {
    /// Condition type: Ready, SyncHealthy, TokenValid.
    pub r#type: String,
    /// True, False, or Unknown.
    pub status: String,
    /// Human-readable reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Human-readable message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Last transition time (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_transition_time: Option<String>,
}
