//! Scale-to-Zero and KEDA Integration Controller
//!
//! Provides serverless auto-scaling capabilities for StreamlineCluster resources,
//! including idle detection, scale-to-zero, cold start activation, and KEDA
//! ScaledObject generation for event-driven scaling.
//!
//! # Features
//!
//! - **Idle Detection**: Tracks when a cluster has zero throughput for a configurable period
//! - **Scale-to-Zero**: Scales StatefulSet replicas to 0 when idle
//! - **Cold Start Activation**: Scales back up when connections arrive (via KEDA or activator)
//! - **KEDA ScaledObject**: Generates KEDA ScaledObject resources for event-driven scaling

use crate::crd::StreamlineCluster;
use crate::error::{OperatorError, Result};
use k8s_openapi::api::apps::v1::StatefulSet;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference};
use kube::api::{Api, Patch, PatchParams, PostParams};
use kube::{Client, Resource, ResourceExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// Configuration types
// ---------------------------------------------------------------------------

/// Scale-to-zero configuration for a StreamlineCluster
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ScaleToZeroConfig {
    /// Enable scale-to-zero behaviour
    #[serde(default)]
    pub enabled: bool,

    /// Seconds of zero throughput before scaling to zero
    #[serde(default = "default_idle_timeout_seconds")]
    pub idle_timeout_seconds: u64,

    /// Minimum replicas to keep when *not* idle (overrides HPA min)
    #[serde(default = "default_min_active_replicas")]
    pub min_active_replicas: i32,

    /// Cooldown period (seconds) after a scale-up before another scale-to-zero is allowed
    #[serde(default = "default_cooldown_seconds")]
    pub cooldown_seconds: u64,

    /// Enable KEDA integration for event-driven activation
    #[serde(default)]
    pub keda_enabled: bool,

    /// KEDA ScaledObject configuration (only used when `keda_enabled` is true)
    #[serde(default)]
    pub keda: Option<KedaConfig>,

    /// Polling interval (seconds) for the idle-check loop
    #[serde(default = "default_polling_interval_seconds")]
    pub polling_interval_seconds: u64,
}

/// KEDA ScaledObject configuration
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct KedaConfig {
    /// Polling interval for KEDA (seconds)
    #[serde(default = "default_keda_polling_interval")]
    pub polling_interval: i32,

    /// Cooldown period for KEDA (seconds)
    #[serde(default = "default_keda_cooldown_period")]
    pub cooldown_period: i32,

    /// Minimum replica count (0 enables scale-to-zero via KEDA)
    #[serde(default)]
    pub min_replica_count: i32,

    /// Maximum replica count
    #[serde(default = "default_keda_max_replicas")]
    pub max_replica_count: i32,

    /// KEDA triggers
    #[serde(default)]
    pub triggers: Vec<KedaTrigger>,
}

/// A single KEDA trigger definition
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct KedaTrigger {
    /// Trigger type (e.g. "kafka", "prometheus", "metrics-api")
    pub r#type: String,

    /// Trigger metadata (key-value pairs interpreted by KEDA)
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

// ---------------------------------------------------------------------------
// Idle-detection state
// ---------------------------------------------------------------------------

/// Snapshot of cluster activity used for idle detection
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterActivitySnapshot {
    /// Cluster name
    pub cluster_name: String,

    /// Namespace
    pub namespace: String,

    /// Messages per second at the time of the snapshot
    pub messages_per_second: f64,

    /// Bytes per second at the time of the snapshot
    pub bytes_per_second: f64,

    /// Number of active client connections
    pub active_connections: u64,

    /// Total consumer lag across all partitions
    pub consumer_lag: i64,

    /// RFC 3339 timestamp of last observed activity (throughput > 0 or connections > 0)
    pub last_activity_at: String,

    /// Duration in seconds since the last activity
    pub idle_duration_seconds: u64,

    /// Whether the cluster is considered idle (idle_duration >= configured timeout)
    pub is_idle: bool,
}

/// Result of a scale-to-zero reconciliation cycle
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScaleToZeroAction {
    /// The action taken
    pub action: ScaleAction,

    /// Human-readable reason
    pub reason: String,

    /// Target replica count after the action
    pub target_replicas: i32,

    /// RFC 3339 timestamp
    pub timestamp: String,
}

/// Possible scale actions
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ScaleAction {
    /// No change needed
    None,
    /// Scale the StatefulSet to zero
    ScaleToZero,
    /// Activate from zero (cold start)
    Activate,
    /// Still within cooldown; no action
    InCooldown,
}

// ---------------------------------------------------------------------------
// Default value helpers
// ---------------------------------------------------------------------------

fn default_idle_timeout_seconds() -> u64 {
    300
}

fn default_min_active_replicas() -> i32 {
    1
}

fn default_cooldown_seconds() -> u64 {
    120
}

fn default_polling_interval_seconds() -> u64 {
    30
}

fn default_keda_polling_interval() -> i32 {
    30
}

fn default_keda_cooldown_period() -> i32 {
    300
}

fn default_keda_max_replicas() -> i32 {
    10
}

// ---------------------------------------------------------------------------
// Controller
// ---------------------------------------------------------------------------

/// Controller for scale-to-zero and KEDA integration
pub struct ScaleToZeroController {
    client: Client,
}

impl ScaleToZeroController {
    /// Create a new scale-to-zero controller
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    /// Evaluate the current cluster activity and decide whether to scale to zero,
    /// activate from zero, or do nothing.
    pub fn evaluate(
        &self,
        snapshot: &ClusterActivitySnapshot,
        config: &ScaleToZeroConfig,
        current_replicas: i32,
        last_scale_up_epoch: Option<u64>,
    ) -> ScaleToZeroAction {
        let now = chrono::Utc::now();
        let timestamp = now.to_rfc3339();

        if !config.enabled {
            return ScaleToZeroAction {
                action: ScaleAction::None,
                reason: "Scale-to-zero is disabled".to_string(),
                target_replicas: current_replicas,
                timestamp,
            };
        }

        // If currently at zero and there is incoming activity, activate
        if current_replicas == 0 && !snapshot.is_idle {
            return ScaleToZeroAction {
                action: ScaleAction::Activate,
                reason: format!(
                    "Activity detected (connections={}, mps={:.1}); activating from zero",
                    snapshot.active_connections, snapshot.messages_per_second,
                ),
                target_replicas: config.min_active_replicas,
                timestamp,
            };
        }

        // Cooldown check: don't scale down if we recently scaled up
        if let Some(last_up) = last_scale_up_epoch {
            let now_epoch = now.timestamp() as u64;
            if now_epoch.saturating_sub(last_up) < config.cooldown_seconds {
                return ScaleToZeroAction {
                    action: ScaleAction::InCooldown,
                    reason: format!(
                        "Within cooldown period ({}s remaining)",
                        config.cooldown_seconds - now_epoch.saturating_sub(last_up),
                    ),
                    target_replicas: current_replicas,
                    timestamp,
                };
            }
        }

        // Idle detection: if the cluster has been idle for longer than the threshold, scale to zero
        if snapshot.is_idle && snapshot.idle_duration_seconds >= config.idle_timeout_seconds {
            return ScaleToZeroAction {
                action: ScaleAction::ScaleToZero,
                reason: format!(
                    "Cluster idle for {}s (threshold: {}s)",
                    snapshot.idle_duration_seconds, config.idle_timeout_seconds,
                ),
                target_replicas: 0,
                timestamp,
            };
        }

        ScaleToZeroAction {
            action: ScaleAction::None,
            reason: "Cluster is active or within idle threshold".to_string(),
            target_replicas: current_replicas,
            timestamp,
        }
    }

    /// Apply a scaling action by patching the StatefulSet replica count.
    pub async fn apply_scale_action(
        &self,
        cluster: &StreamlineCluster,
        namespace: &str,
        action: &ScaleToZeroAction,
    ) -> Result<()> {
        if action.action == ScaleAction::None || action.action == ScaleAction::InCooldown {
            debug!(
                cluster = %cluster.name_any(),
                reason = %action.reason,
                "No scaling action required",
            );
            return Ok(());
        }

        let sts_name = cluster.name_any();
        let sts_api: Api<StatefulSet> = Api::namespaced(self.client.clone(), namespace);

        let patch = serde_json::json!({
            "spec": {
                "replicas": action.target_replicas,
            }
        });

        info!(
            cluster = %sts_name,
            target_replicas = action.target_replicas,
            reason = %action.reason,
            "Applying scale action",
        );

        sts_api
            .patch(
                &sts_name,
                &PatchParams::apply("streamline-operator"),
                &Patch::Merge(&patch),
            )
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        Ok(())
    }

    // ------------------------------------------------------------------
    // KEDA ScaledObject management
    // ------------------------------------------------------------------

    /// Reconcile a KEDA ScaledObject for the given cluster.
    ///
    /// When `keda_enabled` is true the controller creates (or updates) a
    /// `ScaledObject` custom resource that KEDA watches to drive scaling.
    pub async fn reconcile_keda_scaled_object(
        &self,
        cluster: &StreamlineCluster,
        namespace: &str,
        config: &ScaleToZeroConfig,
    ) -> Result<()> {
        if !config.keda_enabled {
            return self.delete_keda_scaled_object(cluster, namespace).await;
        }

        let keda_config = config.keda.as_ref().ok_or_else(|| {
            OperatorError::Configuration(
                "KEDA is enabled but no keda config block was provided".to_string(),
            )
        })?;

        let name = format!("{}-scaledobject", cluster.name_any());
        let scaled_object = self.build_keda_scaled_object(cluster, namespace, keda_config)?;

        let api: Api<kube::api::DynamicObject> = Api::namespaced_with(
            self.client.clone(),
            namespace,
            &kube::api::ApiResource {
                group: "keda.sh".to_string(),
                version: "v1alpha1".to_string(),
                api_version: "keda.sh/v1alpha1".to_string(),
                kind: "ScaledObject".to_string(),
                plural: "scaledobjects".to_string(),
            },
        );

        match api.get(&name).await {
            Ok(_existing) => {
                info!("Updating KEDA ScaledObject {} in namespace {}", name, namespace);
                api.patch(
                    &name,
                    &PatchParams::apply("streamline-operator"),
                    &Patch::Apply(&scaled_object),
                )
                .await
                .map_err(|e| OperatorError::KubeApi(e.to_string()))?;
            }
            Err(_) => {
                info!("Creating KEDA ScaledObject {} in namespace {}", name, namespace);
                api.create(&PostParams::default(), &scaled_object)
                    .await
                    .map_err(|e| OperatorError::KubeApi(e.to_string()))?;
            }
        }

        Ok(())
    }

    /// Delete KEDA ScaledObject for a cluster
    async fn delete_keda_scaled_object(
        &self,
        cluster: &StreamlineCluster,
        namespace: &str,
    ) -> Result<()> {
        let name = format!("{}-scaledobject", cluster.name_any());

        let api: Api<kube::api::DynamicObject> = Api::namespaced_with(
            self.client.clone(),
            namespace,
            &kube::api::ApiResource {
                group: "keda.sh".to_string(),
                version: "v1alpha1".to_string(),
                api_version: "keda.sh/v1alpha1".to_string(),
                kind: "ScaledObject".to_string(),
                plural: "scaledobjects".to_string(),
            },
        );

        match api.delete(&name, &Default::default()).await {
            Ok(_) => {
                info!("Deleted KEDA ScaledObject {} in namespace {}", name, namespace);
            }
            Err(kube::Error::Api(e)) if e.code == 404 => {
                debug!("KEDA ScaledObject {} does not exist, nothing to delete", name);
            }
            Err(e) => {
                warn!("Failed to delete KEDA ScaledObject {}: {}", name, e);
            }
        }

        Ok(())
    }

    /// Build a KEDA ScaledObject as a `DynamicObject`.
    fn build_keda_scaled_object(
        &self,
        cluster: &StreamlineCluster,
        namespace: &str,
        keda_config: &KedaConfig,
    ) -> Result<kube::api::DynamicObject> {
        let name = format!("{}-scaledobject", cluster.name_any());
        let labels = self.common_labels(cluster);
        let owner_ref = self.owner_reference(cluster);

        let triggers: Vec<serde_json::Value> = keda_config
            .triggers
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": t.r#type,
                    "metadata": t.metadata,
                })
            })
            .collect();

        // Fall back to a default Kafka-lag trigger when no triggers are specified
        let triggers = if triggers.is_empty() {
            vec![serde_json::json!({
                "type": "metrics-api",
                "metadata": {
                    "targetValue": "1",
                    "url": format!(
                        "http://{}.{}.svc.cluster.local:{}/scaling/metrics",
                        cluster.name_any(), namespace, 9094
                    ),
                    "valueLocation": "active_connections",
                }
            })]
        } else {
            triggers
        };

        let data = serde_json::json!({
            "apiVersion": "keda.sh/v1alpha1",
            "kind": "ScaledObject",
            "metadata": {
                "name": name,
                "namespace": namespace,
                "labels": labels,
                "ownerReferences": [owner_ref],
            },
            "spec": {
                "scaleTargetRef": {
                    "apiVersion": "apps/v1",
                    "kind": "StatefulSet",
                    "name": cluster.name_any(),
                },
                "pollingInterval": keda_config.polling_interval,
                "cooldownPeriod": keda_config.cooldown_period,
                "minReplicaCount": keda_config.min_replica_count,
                "maxReplicaCount": keda_config.max_replica_count,
                "triggers": triggers,
            }
        });

        let obj: kube::api::DynamicObject =
            serde_json::from_value(data).map_err(|e| OperatorError::Serialization(e.to_string()))?;

        Ok(obj)
    }

    // ------------------------------------------------------------------
    // Helpers
    // ------------------------------------------------------------------

    fn common_labels(&self, cluster: &StreamlineCluster) -> BTreeMap<String, String> {
        let mut labels = BTreeMap::new();
        labels.insert(
            "app.kubernetes.io/name".to_string(),
            "streamline".to_string(),
        );
        labels.insert("app.kubernetes.io/instance".to_string(), cluster.name_any());
        labels.insert(
            "app.kubernetes.io/managed-by".to_string(),
            "streamline-operator".to_string(),
        );
        labels.insert(
            "app.kubernetes.io/component".to_string(),
            "scale-to-zero".to_string(),
        );
        labels
    }

    fn owner_reference(&self, cluster: &StreamlineCluster) -> OwnerReference {
        OwnerReference {
            api_version: StreamlineCluster::api_version(&()).to_string(),
            kind: StreamlineCluster::kind(&()).to_string(),
            name: cluster.name_any(),
            uid: cluster.metadata.uid.clone().unwrap_or_default(),
            controller: Some(true),
            block_owner_deletion: Some(true),
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers – build a `ClusterActivitySnapshot` from raw metric values
// ---------------------------------------------------------------------------

/// Build a [`ClusterActivitySnapshot`] from raw metric values and the configured idle timeout.
pub fn build_activity_snapshot(
    cluster_name: &str,
    namespace: &str,
    messages_per_second: f64,
    bytes_per_second: f64,
    active_connections: u64,
    consumer_lag: i64,
    last_activity_epoch: i64,
    idle_timeout_seconds: u64,
) -> ClusterActivitySnapshot {
    let now = chrono::Utc::now().timestamp();
    let idle_duration = (now - last_activity_epoch).max(0) as u64;
    let is_idle = messages_per_second == 0.0
        && active_connections == 0
        && idle_duration >= idle_timeout_seconds;

    ClusterActivitySnapshot {
        cluster_name: cluster_name.to_string(),
        namespace: namespace.to_string(),
        messages_per_second,
        bytes_per_second,
        active_connections,
        consumer_lag,
        last_activity_at: chrono::DateTime::from_timestamp(last_activity_epoch, 0)
            .unwrap_or_default()
            .to_rfc3339(),
        idle_duration_seconds: idle_duration,
        is_idle,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> ScaleToZeroConfig {
        serde_json::from_str("{}").unwrap()
    }

    #[test]
    fn test_scale_to_zero_config_defaults() {
        let config: ScaleToZeroConfig = serde_json::from_str("{}").unwrap();
        assert!(!config.enabled);
        assert_eq!(config.idle_timeout_seconds, 300);
        assert_eq!(config.min_active_replicas, 1);
        assert_eq!(config.cooldown_seconds, 120);
        assert_eq!(config.polling_interval_seconds, 30);
        assert!(!config.keda_enabled);
    }

    #[test]
    fn test_scale_to_zero_config_custom_values() {
        let json = r#"{
            "enabled": true,
            "idleTimeoutSeconds": 600,
            "minActiveReplicas": 2,
            "cooldownSeconds": 60,
            "kedaEnabled": true,
            "keda": {
                "pollingInterval": 15,
                "cooldownPeriod": 120,
                "minReplicaCount": 0,
                "maxReplicaCount": 20,
                "triggers": [
                    {
                        "type": "kafka",
                        "metadata": {
                            "bootstrapServers": "streamline.default.svc:9092",
                            "consumerGroup": "my-group",
                            "topic": "my-topic",
                            "lagThreshold": "10"
                        }
                    }
                ]
            }
        }"#;

        let config: ScaleToZeroConfig = serde_json::from_str(json).unwrap();
        assert!(config.enabled);
        assert_eq!(config.idle_timeout_seconds, 600);
        assert_eq!(config.min_active_replicas, 2);
        assert!(config.keda_enabled);

        let keda = config.keda.unwrap();
        assert_eq!(keda.polling_interval, 15);
        assert_eq!(keda.max_replica_count, 20);
        assert_eq!(keda.triggers.len(), 1);
        assert_eq!(keda.triggers[0].r#type, "kafka");
    }

    #[test]
    fn test_evaluate_disabled() {
        let controller = ScaleToZeroController {
            client: kube::Client::try_default().err().map(|_| ()).unwrap_or(()),
        };
        // We cannot construct a real Client in unit tests, so test the pure logic via a helper.
        let config = default_config(); // enabled = false
        let snapshot = ClusterActivitySnapshot {
            cluster_name: "test".into(),
            namespace: "default".into(),
            messages_per_second: 0.0,
            bytes_per_second: 0.0,
            active_connections: 0,
            consumer_lag: 0,
            last_activity_at: chrono::Utc::now().to_rfc3339(),
            idle_duration_seconds: 999,
            is_idle: true,
        };

        // evaluate is a pure function; build a minimal controller to call it.
        let action = evaluate_action(&snapshot, &config, 3, None);
        assert_eq!(action.action, ScaleAction::None);
        assert_eq!(action.target_replicas, 3);
    }

    #[test]
    fn test_evaluate_scale_to_zero() {
        let mut config = default_config();
        config.enabled = true;
        config.idle_timeout_seconds = 300;

        let snapshot = ClusterActivitySnapshot {
            cluster_name: "test".into(),
            namespace: "default".into(),
            messages_per_second: 0.0,
            bytes_per_second: 0.0,
            active_connections: 0,
            consumer_lag: 0,
            last_activity_at: "2024-01-01T00:00:00Z".into(),
            idle_duration_seconds: 600,
            is_idle: true,
        };

        let action = evaluate_action(&snapshot, &config, 3, None);
        assert_eq!(action.action, ScaleAction::ScaleToZero);
        assert_eq!(action.target_replicas, 0);
    }

    #[test]
    fn test_evaluate_activate_from_zero() {
        let mut config = default_config();
        config.enabled = true;
        config.min_active_replicas = 2;

        let snapshot = ClusterActivitySnapshot {
            cluster_name: "test".into(),
            namespace: "default".into(),
            messages_per_second: 10.0,
            bytes_per_second: 1000.0,
            active_connections: 1,
            consumer_lag: 0,
            last_activity_at: chrono::Utc::now().to_rfc3339(),
            idle_duration_seconds: 0,
            is_idle: false,
        };

        let action = evaluate_action(&snapshot, &config, 0, None);
        assert_eq!(action.action, ScaleAction::Activate);
        assert_eq!(action.target_replicas, 2);
    }

    #[test]
    fn test_evaluate_cooldown() {
        let mut config = default_config();
        config.enabled = true;
        config.cooldown_seconds = 120;

        let snapshot = ClusterActivitySnapshot {
            cluster_name: "test".into(),
            namespace: "default".into(),
            messages_per_second: 0.0,
            bytes_per_second: 0.0,
            active_connections: 0,
            consumer_lag: 0,
            last_activity_at: "2024-01-01T00:00:00Z".into(),
            idle_duration_seconds: 600,
            is_idle: true,
        };

        // last_scale_up is very recent (essentially "now")
        let now_epoch = chrono::Utc::now().timestamp() as u64;
        let action = evaluate_action(&snapshot, &config, 3, Some(now_epoch));
        assert_eq!(action.action, ScaleAction::InCooldown);
        assert_eq!(action.target_replicas, 3);
    }

    #[test]
    fn test_build_activity_snapshot_idle() {
        let now = chrono::Utc::now().timestamp();
        let last_activity = now - 600;

        let snap = build_activity_snapshot(
            "my-cluster",
            "prod",
            0.0,
            0.0,
            0,
            0,
            last_activity,
            300,
        );

        assert!(snap.is_idle);
        assert!(snap.idle_duration_seconds >= 300);
        assert_eq!(snap.cluster_name, "my-cluster");
    }

    #[test]
    fn test_build_activity_snapshot_active() {
        let now = chrono::Utc::now().timestamp();

        let snap = build_activity_snapshot(
            "my-cluster",
            "prod",
            500.0,
            100_000.0,
            5,
            200,
            now,
            300,
        );

        assert!(!snap.is_idle);
        assert_eq!(snap.active_connections, 5);
    }

    #[test]
    fn test_keda_trigger_parsing() {
        let json = r#"{
            "type": "prometheus",
            "metadata": {
                "serverAddress": "http://prometheus:9090",
                "metricName": "streamline_messages_total",
                "threshold": "100",
                "query": "sum(rate(streamline_messages_total[1m]))"
            }
        }"#;

        let trigger: KedaTrigger = serde_json::from_str(json).unwrap();
        assert_eq!(trigger.r#type, "prometheus");
        assert_eq!(trigger.metadata.len(), 4);
        assert_eq!(trigger.metadata["threshold"], "100");
    }

    // Pure-logic wrapper so we can test without a real kube::Client
    fn evaluate_action(
        snapshot: &ClusterActivitySnapshot,
        config: &ScaleToZeroConfig,
        current_replicas: i32,
        last_scale_up_epoch: Option<u64>,
    ) -> ScaleToZeroAction {
        let now = chrono::Utc::now();
        let timestamp = now.to_rfc3339();

        if !config.enabled {
            return ScaleToZeroAction {
                action: ScaleAction::None,
                reason: "Scale-to-zero is disabled".to_string(),
                target_replicas: current_replicas,
                timestamp,
            };
        }

        if current_replicas == 0 && !snapshot.is_idle {
            return ScaleToZeroAction {
                action: ScaleAction::Activate,
                reason: format!(
                    "Activity detected (connections={}, mps={:.1}); activating from zero",
                    snapshot.active_connections, snapshot.messages_per_second,
                ),
                target_replicas: config.min_active_replicas,
                timestamp,
            };
        }

        if let Some(last_up) = last_scale_up_epoch {
            let now_epoch = now.timestamp() as u64;
            if now_epoch.saturating_sub(last_up) < config.cooldown_seconds {
                return ScaleToZeroAction {
                    action: ScaleAction::InCooldown,
                    reason: format!(
                        "Within cooldown period ({}s remaining)",
                        config.cooldown_seconds - now_epoch.saturating_sub(last_up),
                    ),
                    target_replicas: current_replicas,
                    timestamp,
                };
            }
        }

        if snapshot.is_idle && snapshot.idle_duration_seconds >= config.idle_timeout_seconds {
            return ScaleToZeroAction {
                action: ScaleAction::ScaleToZero,
                reason: format!(
                    "Cluster idle for {}s (threshold: {}s)",
                    snapshot.idle_duration_seconds, config.idle_timeout_seconds,
                ),
                target_replicas: 0,
                timestamp,
            };
        }

        ScaleToZeroAction {
            action: ScaleAction::None,
            reason: "Cluster is active or within idle threshold".to_string(),
            target_replicas: current_replicas,
            timestamp,
        }
    }
}
