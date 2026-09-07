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
    /// Number of Streamline broker replicas.
    ///
    /// Defaults to 1: this operator renders each replica as a standalone
    /// broker and does not yet bootstrap raft peers, so a multi-replica value
    /// produces independent brokers rather than a quorum. Raise it only when
    /// you bootstrap clustering yourself (see README, "Clustering status").
    #[serde(default = "default_replicas")]
    pub replicas: i32,

    /// Container image to use for the Streamline broker.
    ///
    /// This is the Streamline *server* image, not the operator image.
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

    /// Node selector for pod placement.
    ///
    /// Applied: rendered onto the pod template as `spec.nodeSelector`.
    #[serde(default)]
    pub node_selector: std::collections::BTreeMap<String, String>,

    /// Tolerations for pod scheduling.
    ///
    /// Not applied: the operator renders no `tolerations` onto the pod
    /// template, so a non-empty list is rejected rather than silently
    /// dropped. Use `nodeSelector`, which is rendered.
    #[serde(default)]
    pub tolerations: Vec<Toleration>,

    /// Spread brokers across nodes with pod anti-affinity.
    ///
    /// Not applied: the operator renders no `affinity` onto the pod template,
    /// so `true` is rejected rather than silently dropped. It also buys
    /// nothing today — `replicas` is capped at 1, so there is no second pod to
    /// spread.
    #[serde(default)]
    pub pod_anti_affinity: bool,

    /// Rack awareness configuration.
    ///
    /// Not applied: the operator renders no topology spread or rack labelling,
    /// so `enabled: true` is rejected rather than silently dropped.
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

    /// Auto-scaling configuration.
    ///
    /// Enabling autoscaling is rejected until the operator can bootstrap a
    /// real multi-broker cluster. Scaling the current standalone StatefulSet
    /// would create independent brokers and make the HPA fight the controller
    /// over `spec.replicas`.
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
    /// Skip TLS certificate verification (development only, NOT recommended for production)
    #[serde(default)]
    pub insecure_skip_verify: bool,
}

/// Environment variable
///
/// Mirrors the shape of a Kubernetes `EnvVar`: exactly one of `value` (a
/// literal) or `valueFrom` (a reference) must be set, and a `valueFrom` block
/// must declare exactly one of the references below. Anything else is rejected
/// by the operator rather than rendered as an empty variable.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvVar {
    /// Environment variable name
    pub name: String,
    /// Literal value; mutually exclusive with valueFrom. Use "" for a
    /// deliberately empty variable.
    #[serde(default)]
    pub value: Option<String>,
    /// Reference the value is read from; mutually exclusive with value
    #[serde(default)]
    pub value_from: Option<EnvVarSource>,
}

/// Source for environment variable value
///
/// Exactly one reference must be set. The operator maps it straight onto the
/// container's `EnvVarSource`, so the kubelet resolves it at pod start and the
/// operator never reads the referenced Secret or ConfigMap itself.
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

/// The single reference an [`EnvVarSource`] resolves to.
///
/// Returned by [`EnvVar::resolve_source`] so validation and StatefulSet
/// rendering agree, by construction, on which shapes are supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvVarRef<'a> {
    /// `valueFrom.secretKeyRef`
    Secret(&'a SecretKeyRef),
    /// `valueFrom.configMapKeyRef`
    ConfigMap(&'a ConfigMapKeyRef),
}

impl EnvVar {
    /// Resolve what this entry renders to, or describe why it cannot be
    /// rendered faithfully.
    ///
    /// `Ok(None)` is a literal `value`. `Ok(Some(_))` is a reference the
    /// operator can map onto a container `EnvVarSource` verbatim. `Err` means
    /// the entry declares something that cannot be mapped exactly — rendering
    /// it anyway would silently substitute an empty value for, say, a
    /// password.
    pub fn resolve_source(&self) -> std::result::Result<Option<EnvVarRef<'_>>, String> {
        if self.name.trim().is_empty() {
            return Err("name must not be empty".to_string());
        }

        let Some(source) = self.value_from.as_ref() else {
            // A deliberately empty variable is written `value: ""`. An entry
            // carrying neither is the shape that used to reach the broker as
            // an empty string after `valueFrom` was dropped, so it is rejected
            // rather than rendered.
            return if self.value.is_some() {
                Ok(None)
            } else {
                Err(
                    "must set value or valueFrom (use value: \"\" for a deliberately \
                     empty variable)"
                        .to_string(),
                )
            };
        };

        if self.value.is_some() {
            return Err("value and valueFrom are mutually exclusive".to_string());
        }

        match (&source.secret_key_ref, &source.config_map_key_ref) {
            (Some(_), Some(_)) => {
                Err("valueFrom must set exactly one of secretKeyRef or configMapKeyRef".to_string())
            }
            (Some(secret), None) => {
                incomplete_ref("secretKeyRef", &secret.name, &secret.key)?;
                Ok(Some(EnvVarRef::Secret(secret)))
            }
            (None, Some(config_map)) => {
                incomplete_ref("configMapKeyRef", &config_map.name, &config_map.key)?;
                Ok(Some(EnvVarRef::ConfigMap(config_map)))
            }
            (None, None) => Err(
                "valueFrom must set secretKeyRef or configMapKeyRef; an empty valueFrom \
                 would render the variable as an empty string"
                    .to_string(),
            ),
        }
    }
}

/// Reject a key reference whose name or key is blank.
///
/// Both are required by the schema, so only whitespace-only values reach here —
/// and a blank name or key resolves to nothing at pod start.
fn incomplete_ref(field: &str, name: &str, key: &str) -> std::result::Result<(), String> {
    if name.trim().is_empty() {
        return Err(format!("valueFrom.{field}.name must not be empty"));
    }
    if key.trim().is_empty() {
        return Err(format!("valueFrom.{field}.key must not be empty"));
    }
    Ok(())
}

/// Reference to a secret key
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SecretKeyRef {
    /// Name of the secret
    pub name: String,
    /// Key within the secret
    pub key: String,
}

/// Reference to a configmap key
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
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
// `PartialEq` is derived so the controller can compare the status it wants to
// publish against the one already on the object and skip the patch when
// nothing changed; patching an identical status re-triggers the watch and
// spins the reconcile loop. Kept as a plain comment: doc comments become the
// user-facing `description` in the generated CRD schema.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default, PartialEq, Eq)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
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

/// Conservative default: the operator does not bootstrap raft peers, so a
/// single broker is the only replica count it can render a working cluster for.
fn default_replicas() -> i32 {
    1
}

/// Default Streamline *server* image.
///
/// Pinned (not `latest`) and kept in sync with the server image the integration
/// harness exercises — see `tests/integration.rs::DEFAULT_IMAGE`, which
/// `tests/static_manifests.rs` asserts against.
fn default_image() -> String {
    "ghcr.io/streamlinelabs/streamline:0.4.0".to_string()
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

impl ClusterSpec {
    /// Validate the cluster specification, returning errors for invalid values.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        if self.replicas < 1 {
            errors.push(format!("replicas must be >= 1, got {}", self.replicas));
        }
        if self.replicas > 1 {
            // The v0.3.0 CRD defaulted this to 3, so an untouched resource from
            // that release arrives here. The message therefore has to explain
            // an upgrade, not just a bad value.
            errors.push(format!(
                "replicas={} is not supported: the operator does not bootstrap \
                 Streamline raft peers yet (set replicas: 1), so {} independent brokers \
                 would be rendered instead of a quorum.{}",
                self.replicas,
                self.replicas,
                crate::upgrade::remediation_for(
                    "spec.replicas",
                    self.replicas == crate::upgrade::LEGACY_CLUSTER_REPLICAS
                )
            ));
        }

        if self.kafka_port < 1 || self.kafka_port > 65535 {
            errors.push(format!(
                "kafkaPort must be 1-65535, got {}",
                self.kafka_port
            ));
        }
        if self.http_port < 1 || self.http_port > 65535 {
            errors.push(format!("httpPort must be 1-65535, got {}", self.http_port));
        }
        if self.raft_port < 1 || self.raft_port > 65535 {
            errors.push(format!("raftPort must be 1-65535, got {}", self.raft_port));
        }

        if self.kafka_port == self.http_port
            || self.kafka_port == self.raft_port
            || self.http_port == self.raft_port
        {
            errors.push("kafkaPort, httpPort, and raftPort must all be different".to_string());
        }

        // Environment entries the operator cannot map exactly onto a container
        // `EnvVar` are rejected here, so `build_statefulset` never has to
        // choose between dropping a reference and rendering an empty value.
        for (index, env) in self.env.iter().enumerate() {
            if let Err(reason) = env.resolve_source() {
                let named = if env.name.trim().is_empty() {
                    String::new()
                } else {
                    format!(" ({})", env.name)
                };
                errors.push(format!("env[{index}]{named}: {reason}"));
            }
        }

        // Scheduling settings the pod template does not carry. Rendering
        // nothing while reporting `Ready` is what made these advertised but
        // inert; rejecting them keeps the CRD honest until the controller
        // actually renders affinity, topology spread, and tolerations.
        if self.pod_anti_affinity {
            errors.push(format!(
                "podAntiAffinity is not supported: the operator renders no affinity rules \
                 onto the broker pod template (omit it or set it to false; with replicas \
                 capped at 1 there is no second pod to spread).{}",
                // `true` is both the rejected value and the value v0.3.0
                // persisted, so this is always an upgrade message.
                crate::upgrade::remediation_for(
                    "spec.podAntiAffinity",
                    self.pod_anti_affinity == crate::upgrade::LEGACY_POD_ANTI_AFFINITY
                )
            ));
        }
        if self
            .rack_awareness
            .as_ref()
            .is_some_and(|rack| rack.enabled)
        {
            errors.push(
                "rackAwareness.enabled is not supported: the operator renders no topology \
                 spread constraints or rack labelling"
                    .to_string(),
            );
        }
        if !self.tolerations.is_empty() {
            errors.push(format!(
                "tolerations are not supported: the operator renders none onto the broker \
                 pod template, so the {} declared here would be silently dropped (nodeSelector \
                 is rendered and can be used instead)",
                self.tolerations.len()
            ));
        }

        if let Some(ref autoscaling) = self.autoscaling {
            if autoscaling.enabled {
                errors.push(
                    "autoscaling.enabled is not supported: the operator only renders a \
                     standalone broker and cannot safely scale it above one replica"
                        .to_string(),
                );
                if autoscaling.min_replicas < 1 {
                    errors.push(format!(
                        "autoscaling.minReplicas must be >= 1, got {}",
                        autoscaling.min_replicas
                    ));
                }
                if autoscaling.max_replicas < autoscaling.min_replicas {
                    errors.push(format!(
                        "autoscaling.maxReplicas ({}) must be >= minReplicas ({})",
                        autoscaling.max_replicas, autoscaling.min_replicas
                    ));
                }
                if autoscaling.target_cpu_utilization < 1
                    || autoscaling.target_cpu_utilization > 100
                {
                    errors.push(format!(
                        "autoscaling.targetCpuUtilization must be 1-100, got {}",
                        autoscaling.target_cpu_utilization
                    ));
                }
            }
        }

        if let Some(ref tls) = self.tls {
            if tls.enabled {
                if tls.secret_name.trim().is_empty() {
                    errors.push(
                        "tls.secretName is required when tls.enabled is true: the operator mounts \
                         that Secret at /etc/streamline-tls"
                            .to_string(),
                    );
                }
                if tls.mtls_enabled && tls.ca_secret_name.is_none() {
                    errors.push(
                        "tls.caSecretName is required when tls.mtlsEnabled is true: client \
                         certificates cannot be verified without a CA bundle"
                            .to_string(),
                    );
                }
                if tls.insecure_skip_verify {
                    errors.push(
                        "tls.insecureSkipVerify is not supported: it is a client-side setting and \
                         the operator will not render a broker configuration that disables \
                         certificate verification"
                            .to_string(),
                    );
                }
            } else if tls.mtls_enabled {
                errors.push("tls.mtlsEnabled requires tls.enabled to be true".to_string());
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

#[cfg(test)]
mod tests {
    // unwrap/expect are acceptable in tests; the crate-wide lint targets production code.
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn test_cluster_spec_defaults() {
        let spec: ClusterSpec = serde_json::from_str("{}").unwrap();
        assert_eq!(spec.replicas, 1);
        assert_eq!(spec.kafka_port, 9092);
        assert_eq!(spec.http_port, 9094);
        assert!(spec.metrics_enabled);
    }

    #[test]
    fn test_default_image_is_the_server_not_the_operator() {
        let spec: ClusterSpec = serde_json::from_str("{}").unwrap();
        assert_eq!(spec.image, "ghcr.io/streamlinelabs/streamline:0.4.0");
        assert!(
            !spec.image.contains("streamline-operator"),
            "brokers must not default to the operator image"
        );
    }

    #[test]
    fn test_tls_enabled_requires_secret_name() {
        let spec: ClusterSpec =
            serde_json::from_str(r#"{"tls": {"enabled": true, "secretName": ""}}"#).unwrap();
        let errors = spec.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("tls.secretName")));
    }

    #[test]
    fn test_mtls_requires_ca_secret() {
        let spec: ClusterSpec = serde_json::from_str(
            r#"{"tls": {"enabled": true, "secretName": "certs", "mtlsEnabled": true}}"#,
        )
        .unwrap();
        let errors = spec.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("tls.caSecretName")));
    }

    #[test]
    fn test_insecure_skip_verify_is_rejected() {
        let spec: ClusterSpec = serde_json::from_str(
            r#"{"tls": {"enabled": true, "secretName": "certs", "insecureSkipVerify": true}}"#,
        )
        .unwrap();
        let errors = spec.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("insecureSkipVerify")));
    }

    #[test]
    fn test_autoscaling_is_rejected_until_clustering_is_bootstrapped() {
        let spec: ClusterSpec =
            serde_json::from_str(r#"{"autoscaling": {"enabled": true}}"#).unwrap();
        let errors = spec.validate().unwrap_err();
        assert!(errors
            .iter()
            .any(|e| e.contains("autoscaling.enabled is not supported")));
    }

    // --- spec.env ---------------------------------------------------------
    //
    // `valueFrom` is in the shipped schema, so the API server accepts it. It
    // used to be dropped during rendering, which turned a `secretKeyRef` into
    // an empty environment variable inside the broker. Every shape is now
    // either mapped exactly onto the container's `EnvVarSource` or rejected
    // here.

    #[test]
    fn test_env_literal_and_supported_value_from_shapes_validate() {
        let spec: ClusterSpec = serde_json::from_str(
            r#"{"env": [
                   {"name": "LITERAL", "value": "on"},
                   {"name": "DELIBERATELY_EMPTY", "value": ""},
                   {"name": "FROM_SECRET",
                    "valueFrom": {"secretKeyRef": {"name": "broker-auth", "key": "password"}}},
                   {"name": "FROM_CONFIG_MAP",
                    "valueFrom": {"configMapKeyRef": {"name": "broker-tuning", "key": "flags"}}}
               ]}"#,
        )
        .unwrap();
        assert!(spec.validate().is_ok(), "{:?}", spec.validate());

        assert_eq!(spec.env[0].resolve_source().unwrap(), None);
        assert_eq!(spec.env[1].resolve_source().unwrap(), None);
        assert!(matches!(
            spec.env[2].resolve_source().unwrap(),
            Some(EnvVarRef::Secret(secret)) if secret.name == "broker-auth" && secret.key == "password"
        ));
        assert!(matches!(
            spec.env[3].resolve_source().unwrap(),
            Some(EnvVarRef::ConfigMap(cm)) if cm.name == "broker-tuning" && cm.key == "flags"
        ));
    }

    /// An entry with neither `value` nor `valueFrom` is the shape a dropped
    /// `valueFrom` used to leave behind, so it is rejected; `value: ""` is the
    /// explicit way to ask for an empty variable.
    #[test]
    fn test_env_entry_without_a_value_or_a_source_is_rejected() {
        let spec: ClusterSpec =
            serde_json::from_str(r#"{"env": [{"name": "SASL_PASSWORD"}]}"#).unwrap();
        let errors = spec.validate().unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.contains("env[0]") && e.contains("must set value or valueFrom")),
            "{errors:?}"
        );
    }

    #[test]
    fn test_env_value_and_value_from_are_mutually_exclusive() {
        let spec: ClusterSpec = serde_json::from_str(
            r#"{"env": [{"name": "SASL_PASSWORD", "value": "literal",
                         "valueFrom": {"secretKeyRef": {"name": "s", "key": "k"}}}]}"#,
        )
        .unwrap();
        let errors = spec.validate().unwrap_err();
        assert!(
            errors.iter().any(|e| e.contains("env[0]")
                && e.contains("SASL_PASSWORD")
                && e.contains("mutually exclusive")),
            "{errors:?}"
        );
    }

    #[test]
    fn test_env_value_from_must_declare_exactly_one_reference() {
        let empty: ClusterSpec =
            serde_json::from_str(r#"{"env": [{"name": "SASL_PASSWORD", "valueFrom": {}}]}"#)
                .unwrap();
        let errors = empty.validate().unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.contains("secretKeyRef or configMapKeyRef")),
            "{errors:?}"
        );

        let both: ClusterSpec = serde_json::from_str(
            r#"{"env": [{"name": "SASL_PASSWORD",
                         "valueFrom": {"secretKeyRef": {"name": "s", "key": "k"},
                                       "configMapKeyRef": {"name": "c", "key": "k"}}}]}"#,
        )
        .unwrap();
        let errors = both.validate().unwrap_err();
        assert!(
            errors.iter().any(|e| e.contains("exactly one")),
            "{errors:?}"
        );
    }

    #[test]
    fn test_env_incomplete_references_are_rejected() {
        let cases = [
            (
                r#"{"env": [{"name": "A", "valueFrom": {"secretKeyRef": {"name": "", "key": "k"}}}]}"#,
                "valueFrom.secretKeyRef.name",
            ),
            (
                r#"{"env": [{"name": "A", "valueFrom": {"secretKeyRef": {"name": "s", "key": " "}}}]}"#,
                "valueFrom.secretKeyRef.key",
            ),
            (
                r#"{"env": [{"name": "A", "valueFrom": {"configMapKeyRef": {"name": "", "key": "k"}}}]}"#,
                "valueFrom.configMapKeyRef.name",
            ),
            (
                r#"{"env": [{"name": "A", "valueFrom": {"configMapKeyRef": {"name": "c", "key": ""}}}]}"#,
                "valueFrom.configMapKeyRef.key",
            ),
        ];

        for (json, expected) in cases {
            let spec: ClusterSpec = serde_json::from_str(json).unwrap();
            let errors = spec.validate().unwrap_err();
            assert!(
                errors.iter().any(|e| e.contains(expected)),
                "{json} should be rejected with {expected}, got {errors:?}"
            );
        }
    }

    #[test]
    fn test_env_name_must_not_be_empty() {
        let spec: ClusterSpec =
            serde_json::from_str(r#"{"env": [{"name": "  ", "value": "x"}]}"#).unwrap();
        let errors = spec.validate().unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.contains("env[0]") && e.contains("name must not be empty")),
            "{errors:?}"
        );
    }

    // --- Scheduling -------------------------------------------------------
    //
    // The pod template renders `nodeSelector` and nothing else. Anti-affinity,
    // rack awareness, and tolerations were advertised in the schema, accepted,
    // and then never rendered, so a cluster that asked to be spread across
    // nodes was reported `Ready` while sitting wherever the scheduler put it.

    #[test]
    fn test_pod_anti_affinity_defaults_to_false_and_is_rejected_when_enabled() {
        let default_spec: ClusterSpec = serde_json::from_str("{}").unwrap();
        assert!(
            !default_spec.pod_anti_affinity,
            "the default must be a setting the operator can honour"
        );
        assert!(default_spec.validate().is_ok());

        let enabled: ClusterSpec = serde_json::from_str(r#"{"podAntiAffinity": true}"#).unwrap();
        let errors = enabled.validate().unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.contains("podAntiAffinity is not supported")),
            "{errors:?}"
        );
    }

    #[test]
    fn test_rack_awareness_is_rejected_only_when_enabled() {
        let enabled: ClusterSpec =
            serde_json::from_str(r#"{"rackAwareness": {"enabled": true}}"#).unwrap();
        let errors = enabled.validate().unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.contains("rackAwareness.enabled is not supported")),
            "{errors:?}"
        );

        let disabled: ClusterSpec =
            serde_json::from_str(r#"{"rackAwareness": {"enabled": false}}"#).unwrap();
        assert!(disabled.validate().is_ok(), "{:?}", disabled.validate());
    }

    #[test]
    fn test_tolerations_are_rejected_when_present() {
        let spec: ClusterSpec = serde_json::from_str(
            r#"{"tolerations": [{"key": "dedicated", "operator": "Equal",
                                 "value": "streamline", "effect": "NoSchedule"}]}"#,
        )
        .unwrap();
        let errors = spec.validate().unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.contains("tolerations are not supported")),
            "{errors:?}"
        );

        let empty: ClusterSpec = serde_json::from_str(r#"{"tolerations": []}"#).unwrap();
        assert!(empty.validate().is_ok());
    }

    /// `nodeSelector` *is* rendered, so it must keep validating.
    #[test]
    fn test_node_selector_remains_supported() {
        let spec: ClusterSpec =
            serde_json::from_str(r#"{"nodeSelector": {"disktype": "ssd"}}"#).unwrap();
        assert!(spec.validate().is_ok(), "{:?}", spec.validate());
        assert_eq!(
            spec.node_selector.get("disktype").map(String::as_str),
            Some("ssd")
        );
    }

    #[test]
    fn test_supported_tls_config_validates() {
        let spec: ClusterSpec = serde_json::from_str(
            r#"{"tls": {"enabled": true, "secretName": "certs", "mtlsEnabled": true, "caSecretName": "ca"}}"#,
        )
        .unwrap();
        assert!(spec.validate().is_ok());
    }

    #[test]
    fn test_cluster_spec_validate_defaults_pass() {
        let spec: ClusterSpec = serde_json::from_str("{}").unwrap();
        assert!(spec.validate().is_ok());
    }

    #[test]
    fn test_cluster_spec_validate_negative_replicas() {
        let spec: ClusterSpec = serde_json::from_str(r#"{"replicas": -1}"#).unwrap();
        let result = spec.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err()[0].contains("replicas must be >= 1"));
    }

    #[test]
    fn test_cluster_spec_rejects_multiple_standalone_brokers() {
        let spec: ClusterSpec = serde_json::from_str(r#"{"replicas": 3}"#).unwrap();
        let result = spec.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err()[0].contains("does not bootstrap Streamline raft peers"));
    }

    #[test]
    fn test_cluster_spec_validate_duplicate_ports() {
        let spec: ClusterSpec =
            serde_json::from_str(r#"{"kafkaPort": 9092, "httpPort": 9092}"#).unwrap();
        let result = spec.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err()[0].contains("must all be different"));
    }

    #[test]
    fn test_cluster_spec_validate_invalid_port() {
        let spec: ClusterSpec = serde_json::from_str(r#"{"kafkaPort": 99999}"#).unwrap();
        let result = spec.validate();
        assert!(result.is_err());
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
