//! StreamlineCluster Custom Resource Definition
//!
//! Defines the specification for deploying a Streamline cluster on Kubernetes.

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// StreamlineCluster is the Schema for the streamlineclusters API
#[derive(CustomResource, Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "streamline.io",
    version = "v1alpha1",
    kind = "StreamlineCluster",
    namespaced,
    status = "ClusterStatus",
    shortname = "slc",
    printcolumn = r#"{"name":"Replicas","type":"integer","jsonPath":".spec.replicas"}"#,
    printcolumn = r#"{"name":"Ready","type":"integer","jsonPath":".status.readyReplicas"}"#,
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct ClusterSpec {
    /// Number of Streamline broker replicas
    #[serde(default = "default_replicas")]
    pub replicas: i32,

    /// Container image to use for Streamline
    #[serde(default = "default_image")]
    pub image: String,

    /// Image pull policy
    #[serde(default = "default_pull_policy")]
    pub image_pull_policy: String,

    /// Resource requirements for each broker
    #[serde(default)]
    pub resources: ResourceRequirements,

    /// Storage configuration
    #[serde(default)]
    pub storage: ClusterStorage,

    /// TLS configuration
    #[serde(default)]
    pub tls: Option<ClusterTls>,

    /// Kafka protocol port
    #[serde(default = "default_kafka_port")]
    pub kafka_port: i32,

    /// HTTP/metrics port
    #[serde(default = "default_http_port")]
    pub http_port: i32,

    /// Raft port for cluster communication
    #[serde(default = "default_raft_port")]
    pub raft_port: i32,

    /// Additional environment variables
    #[serde(default)]
    pub env: Vec<EnvVar>,

    /// Node selector for pod placement
    #[serde(default)]
    pub node_selector: std::collections::BTreeMap<String, String>,

    /// Tolerations for pod scheduling
    #[serde(default)]
    pub tolerations: Vec<Toleration>,

    /// Pod anti-affinity to spread across nodes
    #[serde(default = "default_anti_affinity")]
    pub pod_anti_affinity: bool,

    /// Rack awareness configuration
    #[serde(default)]
    pub rack_awareness: Option<RackAwareness>,

    /// Service account name
    #[serde(default)]
    pub service_account_name: Option<String>,

    /// Log level for Streamline
    #[serde(default = "default_log_level")]
    pub log_level: String,

    /// Enable metrics endpoint
    #[serde(default = "default_true")]
    pub metrics_enabled: bool,

    /// Rolling update strategy
    #[serde(default)]
    pub update_strategy: UpdateStrategy,

    /// Auto-scaling configuration
    #[serde(default)]
    pub autoscaling: Option<AutoScalingSpec>,
}

/// Auto-scaling specification for the cluster
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct AutoScalingSpec {
    /// Enable auto-scaling
    #[serde(default)]
    pub enabled: bool,

    /// Minimum number of replicas
    #[serde(default = "default_min_replicas")]
    pub min_replicas: i32,

    /// Maximum number of replicas
    #[serde(default = "default_max_replicas")]
    pub max_replicas: i32,

    /// Target CPU utilization percentage
    #[serde(default = "default_cpu_target")]
    pub target_cpu_utilization: i32,

    /// Target memory utilization percentage
    #[serde(default = "default_memory_target")]
    pub target_memory_utilization: i32,

    /// Enable partition-aware scaling
    #[serde(default)]
    pub partition_aware: bool,

    /// Target consumer lag per partition
    #[serde(default = "default_lag_threshold")]
    pub target_lag_per_partition: i64,

    /// Target messages per second per broker
    #[serde(default = "default_mps_threshold")]
    pub target_messages_per_second: i64,

    /// Scaling behavior configuration
    #[serde(default)]
    pub behavior: Option<ScalingBehaviorSpec>,
}

/// Scaling behavior for up/down operations
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScalingBehaviorSpec {
    /// Scale-up behavior
    #[serde(default)]
    pub scale_up: Option<ScalingRulesSpec>,

    /// Scale-down behavior
    #[serde(default)]
    pub scale_down: Option<ScalingRulesSpec>,
}

/// Scaling rules for up/down operations
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScalingRulesSpec {
    /// Stabilization window in seconds
    #[serde(default)]
    pub stabilization_window_seconds: Option<i32>,

    /// Select policy (Max, Min, Disabled)
    #[serde(default)]
    pub select_policy: Option<String>,

    /// Scaling policies
    #[serde(default)]
    pub policies: Vec<ScalingPolicySpec>,
}

/// Individual scaling policy
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScalingPolicySpec {
    /// Policy type (Pods, Percent)
    pub r#type: String,

    /// Value for the policy
    pub value: i32,

    /// Period in seconds
    pub period_seconds: i32,
}

/// Resource requirements for containers
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ResourceRequirements {
    /// Resource limits
    #[serde(default)]
    pub limits: ResourceList,
    /// Resource requests
    #[serde(default)]
    pub requests: ResourceList,
}

/// Resource quantities
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct ResourceList {
    /// CPU limit/request (e.g., "500m", "2")
    #[serde(default)]
    pub cpu: Option<String>,
    /// Memory limit/request (e.g., "512Mi", "2Gi")
    #[serde(default)]
    pub memory: Option<String>,
}

/// Storage configuration for the cluster
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClusterStorage {
    /// Storage class name for PVCs
    #[serde(default)]
    pub storage_class_name: Option<String>,
    /// Size of the persistent volume (e.g., "10Gi")
    #[serde(default = "default_storage_size")]
    pub size: String,
    /// Access modes for the PVC
    #[serde(default = "default_access_modes")]
    pub access_modes: Vec<String>,
}

impl Default for ClusterStorage {
    fn default() -> Self {
        Self {
            storage_class_name: None,
            size: default_storage_size(),
            access_modes: default_access_modes(),
        }
    }
}

/// TLS configuration
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClusterTls {
    /// Enable TLS
    pub enabled: bool,
    /// Secret containing TLS certificates
    pub secret_name: String,
    /// Enable mTLS (mutual TLS)
    #[serde(default)]
    pub mtls_enabled: bool,
    /// Secret containing CA certificate for mTLS
    #[serde(default)]
    pub ca_secret_name: Option<String>,
}

/// Environment variable
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvVar {
    /// Environment variable name
    pub name: String,
    /// Environment variable value
    #[serde(default)]
    pub value: Option<String>,
    /// Reference to a secret key
    #[serde(default)]
    pub value_from: Option<EnvVarSource>,
}

/// Source for environment variable value
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvVarSource {
    /// Secret key reference
    #[serde(default)]
    pub secret_key_ref: Option<SecretKeyRef>,
    /// ConfigMap key reference
    #[serde(default)]
    pub config_map_key_ref: Option<ConfigMapKeyRef>,
}

/// Reference to a secret key
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SecretKeyRef {
    /// Name of the secret
    pub name: String,
    /// Key within the secret
    pub key: String,
}

/// Reference to a configmap key
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ConfigMapKeyRef {
    /// Name of the configmap
    pub name: String,
    /// Key within the configmap
    pub key: String,
}

/// Toleration for pod scheduling
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Toleration {
    /// Taint key
    #[serde(default)]
    pub key: Option<String>,
    /// Operator (Exists, Equal)
    #[serde(default)]
    pub operator: Option<String>,
    /// Taint value
    #[serde(default)]
    pub value: Option<String>,
    /// Effect (NoSchedule, PreferNoSchedule, NoExecute)
    #[serde(default)]
    pub effect: Option<String>,
    /// Toleration seconds for NoExecute
    #[serde(default)]
    pub toleration_seconds: Option<i64>,
}

/// Rack awareness configuration
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RackAwareness {
    /// Enable rack awareness
    pub enabled: bool,
    /// Label key to use for rack ID
    #[serde(default = "default_rack_label")]
    pub topology_key: String,
}

/// Rolling update strategy
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStrategy {
    /// Update strategy type (RollingUpdate, OnDelete)
    #[serde(default = "default_update_type")]
    pub r#type: String,
    /// Maximum unavailable pods during rolling update
    #[serde(default)]
    pub max_unavailable: Option<i32>,
}

impl Default for UpdateStrategy {
    fn default() -> Self {
        Self {
            r#type: default_update_type(),
            max_unavailable: Some(1),
        }
    }
}

/// Status of the StreamlineCluster
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ClusterStatus {
    /// Current phase of the cluster
    #[serde(default)]
    pub phase: ClusterPhase,
    /// Number of ready replicas
    #[serde(default)]
    pub ready_replicas: i32,
    /// Total number of replicas
    #[serde(default)]
    pub replicas: i32,
    /// Current leader node ID
    #[serde(default)]
    pub leader_id: Option<i32>,
    /// List of broker endpoints
    #[serde(default)]
    pub broker_endpoints: Vec<String>,
    /// Conditions representing cluster state
    #[serde(default)]
    pub conditions: Vec<ClusterCondition>,
    /// Last observed generation
    #[serde(default)]
    pub observed_generation: Option<i64>,
    /// Last update timestamp
    #[serde(default)]
    pub last_updated: Option<String>,
}

/// Phase of the cluster lifecycle
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default, PartialEq, Eq)]
pub enum ClusterPhase {
    /// Cluster is being created
    #[default]
    Pending,
    /// Cluster is running
    Running,
    /// Cluster is being scaled
    Scaling,
    /// Cluster is being upgraded
    Upgrading,
    /// Cluster has failed
    Failed,
    /// Cluster is being deleted
    Terminating,
}

/// Condition of the cluster
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClusterCondition {
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
fn default_replicas() -> i32 {
    3
}

fn default_image() -> String {
    "ghcr.io/streamlinelabs/streamline-operator:latest".to_string()
}

fn default_pull_policy() -> String {
    "IfNotPresent".to_string()
}

fn default_kafka_port() -> i32 {
    9092
}

fn default_http_port() -> i32 {
    9094
}

fn default_raft_port() -> i32 {
    9095
}

fn default_storage_size() -> String {
    "10Gi".to_string()
}

fn default_access_modes() -> Vec<String> {
    vec!["ReadWriteOnce".to_string()]
}

fn default_anti_affinity() -> bool {
    true
}

fn default_rack_label() -> String {
    "topology.kubernetes.io/zone".to_string()
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_true() -> bool {
    true
}

fn default_update_type() -> String {
    "RollingUpdate".to_string()
}

fn default_min_replicas() -> i32 {
    1
}

fn default_max_replicas() -> i32 {
    10
}

fn default_cpu_target() -> i32 {
    70
}

fn default_memory_target() -> i32 {
    80
}

fn default_lag_threshold() -> i64 {
    10000
}

fn default_mps_threshold() -> i64 {
    100000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cluster_spec_defaults() {
        let spec: ClusterSpec = serde_json::from_str("{}").unwrap();
        assert_eq!(spec.replicas, 3);
        assert_eq!(spec.kafka_port, 9092);
        assert_eq!(spec.http_port, 9094);
        assert!(spec.metrics_enabled);
    }

    #[test]
    fn test_cluster_phase_default() {
        let phase = ClusterPhase::default();
        assert_eq!(phase, ClusterPhase::Pending);
    }

    #[test]
    fn test_cluster_storage_defaults() {
        let storage = ClusterStorage::default();
        assert_eq!(storage.size, "10Gi");
        assert_eq!(storage.access_modes, vec!["ReadWriteOnce"]);
    }
}
