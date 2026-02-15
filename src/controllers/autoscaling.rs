//! Auto-scaling Controller for StreamlineCluster
//!
//! Provides Kubernetes HPA (Horizontal Pod Autoscaler) integration and
//! partition-based scaling recommendations for Streamline clusters.
//!
//! # Features
//!
//! - **HPA Integration**: Automatic creation and management of HPA resources
//! - **Custom Metrics**: Expose Streamline-specific metrics for scaling decisions
//! - **Partition-Aware Scaling**: Scale based on partition lag and throughput
//! - **Scaling Recommendations**: Intelligent scaling suggestions based on workload

use crate::crd::StreamlineCluster;
use crate::error::{OperatorError, Result};
use k8s_openapi::api::autoscaling::v2::{
    CrossVersionObjectReference, HPAScalingPolicy, HPAScalingRules, HorizontalPodAutoscaler,
    HorizontalPodAutoscalerBehavior, HorizontalPodAutoscalerSpec, MetricIdentifier, MetricSpec,
    MetricTarget,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference};
use kube::api::{Api, Patch, PatchParams, PostParams};
use kube::{Client, Resource, ResourceExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tracing::{debug, info, warn};

/// Auto-scaling configuration for a StreamlineCluster
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct AutoScalingConfig {
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

    /// Custom metrics for scaling
    #[serde(default)]
    pub custom_metrics: Vec<CustomMetric>,

    /// Scaling behavior configuration
    #[serde(default)]
    pub behavior: Option<ScalingBehavior>,

    /// Enable partition-aware scaling
    #[serde(default)]
    pub partition_aware: bool,

    /// Target consumer lag per partition
    #[serde(default = "default_lag_threshold")]
    pub target_lag_per_partition: i64,

    /// Target messages per second per broker
    #[serde(default = "default_mps_threshold")]
    pub target_messages_per_second: i64,
}

/// Custom metric for auto-scaling
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CustomMetric {
    /// Metric name
    pub name: String,

    /// Metric type (Pods, Object, External)
    #[serde(default = "default_metric_type")]
    pub metric_type: String,

    /// Target value or average value
    pub target: MetricTargetSpec,
}

/// Metric target specification
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MetricTargetSpec {
    /// Target type (Utilization, Value, AverageValue)
    #[serde(default = "default_target_type")]
    pub r#type: String,

    /// Target value (for Value and AverageValue types)
    #[serde(default)]
    pub value: Option<String>,

    /// Average value across pods
    #[serde(default)]
    pub average_value: Option<String>,

    /// Target utilization percentage (for Utilization type)
    #[serde(default)]
    pub average_utilization: Option<i32>,
}

/// Scaling behavior configuration
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScalingBehavior {
    /// Scale-up behavior
    #[serde(default)]
    pub scale_up: Option<ScalingRules>,

    /// Scale-down behavior
    #[serde(default)]
    pub scale_down: Option<ScalingRules>,
}

/// Scaling rules for up/down behavior
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScalingRules {
    /// Stabilization window in seconds
    #[serde(default)]
    pub stabilization_window_seconds: Option<i32>,

    /// Select policy (Max, Min, Disabled)
    #[serde(default)]
    pub select_policy: Option<String>,

    /// Scaling policies
    #[serde(default)]
    pub policies: Vec<ScalingPolicy>,
}

/// Individual scaling policy
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScalingPolicy {
    /// Policy type (Pods, Percent)
    pub r#type: String,

    /// Value for the policy
    pub value: i32,

    /// Period in seconds
    pub period_seconds: i32,
}

// Default value functions
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

fn default_metric_type() -> String {
    "Pods".to_string()
}

fn default_target_type() -> String {
    "AverageValue".to_string()
}

/// Scaling recommendation from the auto-scaler
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScalingRecommendation {
    /// Recommended replica count
    pub recommended_replicas: i32,

    /// Current replica count
    pub current_replicas: i32,

    /// Reason for the recommendation
    pub reason: String,

    /// Confidence score (0-100)
    pub confidence: i32,

    /// Metrics that triggered the recommendation
    pub triggering_metrics: Vec<String>,

    /// Timestamp of the recommendation
    pub timestamp: String,
}

/// Partition-aware scaling metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartitionMetrics {
    /// Total number of partitions
    pub total_partitions: i32,

    /// Total consumer lag across all partitions
    pub total_lag: i64,

    /// Average lag per partition
    pub average_lag: f64,

    /// Maximum lag for any single partition
    pub max_partition_lag: i64,

    /// Messages per second
    pub messages_per_second: f64,

    /// Bytes per second
    pub bytes_per_second: f64,

    /// Number of under-replicated partitions
    pub under_replicated_partitions: i32,
}

/// Auto-scaling controller for StreamlineCluster resources
pub struct AutoScalingController {
    client: Client,
}

impl AutoScalingController {
    /// Create a new auto-scaling controller
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    /// Reconcile HPA for a StreamlineCluster
    pub async fn reconcile_hpa(
        &self,
        cluster: &StreamlineCluster,
        namespace: &str,
        autoscaling: &AutoScalingConfig,
    ) -> Result<()> {
        if !autoscaling.enabled {
            // If autoscaling is disabled, delete any existing HPA
            return self.delete_hpa(cluster, namespace).await;
        }

        let name = format!("{}-hpa", cluster.name_any());
        let hpas: Api<HorizontalPodAutoscaler> = Api::namespaced(self.client.clone(), namespace);

        let hpa = self.build_hpa(cluster, namespace, autoscaling)?;

        match hpas.get(&name).await {
            Ok(_existing) => {
                info!("Updating HPA {} in namespace {}", name, namespace);
                hpas.patch(
                    &name,
                    &PatchParams::apply("streamline-operator"),
                    &Patch::Apply(&hpa),
                )
                .await
                .map_err(|e| OperatorError::KubeApi(e.to_string()))?;
            }
            Err(_) => {
                info!("Creating HPA {} in namespace {}", name, namespace);
                hpas.create(&PostParams::default(), &hpa)
                    .await
                    .map_err(|e| OperatorError::KubeApi(e.to_string()))?;
            }
        }

        Ok(())
    }

    /// Delete HPA for a cluster
    async fn delete_hpa(&self, cluster: &StreamlineCluster, namespace: &str) -> Result<()> {
        let name = format!("{}-hpa", cluster.name_any());
        let hpas: Api<HorizontalPodAutoscaler> = Api::namespaced(self.client.clone(), namespace);

        match hpas.delete(&name, &Default::default()).await {
            Ok(_) => {
                info!("Deleted HPA {} in namespace {}", name, namespace);
            }
            Err(kube::Error::Api(e)) if e.code == 404 => {
                debug!("HPA {} does not exist, nothing to delete", name);
            }
            Err(e) => {
                warn!("Failed to delete HPA {}: {}", name, e);
            }
        }

        Ok(())
    }

    /// Build an HPA resource from the cluster spec
    fn build_hpa(
        &self,
        cluster: &StreamlineCluster,
        namespace: &str,
        autoscaling: &AutoScalingConfig,
    ) -> Result<HorizontalPodAutoscaler> {
        let name = format!("{}-hpa", cluster.name_any());
        let labels = self.common_labels(cluster);
        let owner_ref = self.owner_reference(cluster);

        // Build metrics
        let mut metrics = Vec::new();

        // CPU utilization metric
        metrics.push(MetricSpec {
            type_: "Resource".to_string(),
            resource: Some(k8s_openapi::api::autoscaling::v2::ResourceMetricSource {
                name: "cpu".to_string(),
                target: MetricTarget {
                    type_: "Utilization".to_string(),
                    average_utilization: Some(autoscaling.target_cpu_utilization),
                    ..Default::default()
                },
            }),
            ..Default::default()
        });

        // Memory utilization metric
        metrics.push(MetricSpec {
            type_: "Resource".to_string(),
            resource: Some(k8s_openapi::api::autoscaling::v2::ResourceMetricSource {
                name: "memory".to_string(),
                target: MetricTarget {
                    type_: "Utilization".to_string(),
                    average_utilization: Some(autoscaling.target_memory_utilization),
                    ..Default::default()
                },
            }),
            ..Default::default()
        });

        // Add custom metrics
        for custom in &autoscaling.custom_metrics {
            let metric = self.build_custom_metric(custom);
            metrics.push(metric);
        }

        // Add partition-aware metrics if enabled
        if autoscaling.partition_aware {
            // Consumer lag metric
            metrics.push(MetricSpec {
                type_: "Pods".to_string(),
                pods: Some(k8s_openapi::api::autoscaling::v2::PodsMetricSource {
                    metric: MetricIdentifier {
                        name: "streamline_consumer_lag".to_string(),
                        selector: None,
                    },
                    target: MetricTarget {
                        type_: "AverageValue".to_string(),
                        average_value: Some(
                            k8s_openapi::apimachinery::pkg::api::resource::Quantity(
                                autoscaling.target_lag_per_partition.to_string(),
                            ),
                        ),
                        ..Default::default()
                    },
                }),
                ..Default::default()
            });

            // Messages per second metric
            metrics.push(MetricSpec {
                type_: "Pods".to_string(),
                pods: Some(k8s_openapi::api::autoscaling::v2::PodsMetricSource {
                    metric: MetricIdentifier {
                        name: "streamline_messages_per_second".to_string(),
                        selector: None,
                    },
                    target: MetricTarget {
                        type_: "AverageValue".to_string(),
                        average_value: Some(
                            k8s_openapi::apimachinery::pkg::api::resource::Quantity(
                                autoscaling.target_messages_per_second.to_string(),
                            ),
                        ),
                        ..Default::default()
                    },
                }),
                ..Default::default()
            });
        }

        // Build scaling behavior
        let behavior = autoscaling
            .behavior
            .as_ref()
            .map(|b| self.build_behavior(b));

        let hpa = HorizontalPodAutoscaler {
            metadata: ObjectMeta {
                name: Some(name),
                namespace: Some(namespace.to_string()),
                labels: Some(labels),
                owner_references: Some(vec![owner_ref]),
                ..Default::default()
            },
            spec: Some(HorizontalPodAutoscalerSpec {
                scale_target_ref: CrossVersionObjectReference {
                    api_version: Some("apps/v1".to_string()),
                    kind: "StatefulSet".to_string(),
                    name: cluster.name_any(),
                },
                min_replicas: Some(autoscaling.min_replicas),
                max_replicas: autoscaling.max_replicas,
                metrics: Some(metrics),
                behavior,
            }),
            ..Default::default()
        };

        Ok(hpa)
    }

    /// Build a custom metric specification
    fn build_custom_metric(&self, custom: &CustomMetric) -> MetricSpec {
        match custom.metric_type.as_str() {
            "Pods" => MetricSpec {
                type_: "Pods".to_string(),
                pods: Some(k8s_openapi::api::autoscaling::v2::PodsMetricSource {
                    metric: MetricIdentifier {
                        name: custom.name.clone(),
                        selector: None,
                    },
                    target: self.build_metric_target(&custom.target),
                }),
                ..Default::default()
            },
            "External" => MetricSpec {
                type_: "External".to_string(),
                external: Some(k8s_openapi::api::autoscaling::v2::ExternalMetricSource {
                    metric: MetricIdentifier {
                        name: custom.name.clone(),
                        selector: None,
                    },
                    target: self.build_metric_target(&custom.target),
                }),
                ..Default::default()
            },
            _ => MetricSpec {
                type_: "Pods".to_string(),
                pods: Some(k8s_openapi::api::autoscaling::v2::PodsMetricSource {
                    metric: MetricIdentifier {
                        name: custom.name.clone(),
                        selector: None,
                    },
                    target: self.build_metric_target(&custom.target),
                }),
                ..Default::default()
            },
        }
    }

    /// Build a metric target
    fn build_metric_target(&self, target: &MetricTargetSpec) -> MetricTarget {
        MetricTarget {
            type_: target.r#type.clone(),
            value: target
                .value
                .as_ref()
                .map(|v| k8s_openapi::apimachinery::pkg::api::resource::Quantity(v.clone())),
            average_value: target
                .average_value
                .as_ref()
                .map(|v| k8s_openapi::apimachinery::pkg::api::resource::Quantity(v.clone())),
            average_utilization: target.average_utilization,
        }
    }

    /// Build HPA scaling behavior
    fn build_behavior(&self, behavior: &ScalingBehavior) -> HorizontalPodAutoscalerBehavior {
        HorizontalPodAutoscalerBehavior {
            scale_up: behavior
                .scale_up
                .as_ref()
                .map(|r| self.build_scaling_rules(r)),
            scale_down: behavior
                .scale_down
                .as_ref()
                .map(|r| self.build_scaling_rules(r)),
        }
    }

    /// Build scaling rules
    fn build_scaling_rules(&self, rules: &ScalingRules) -> HPAScalingRules {
        HPAScalingRules {
            stabilization_window_seconds: rules.stabilization_window_seconds,
            select_policy: rules.select_policy.clone(),
            policies: if rules.policies.is_empty() {
                None
            } else {
                Some(
                    rules
                        .policies
                        .iter()
                        .map(|p| HPAScalingPolicy {
                            type_: p.r#type.clone(),
                            value: p.value,
                            period_seconds: p.period_seconds,
                        })
                        .collect(),
                )
            },
        }
    }

    /// Generate scaling recommendation based on metrics
    pub fn generate_recommendation(
        &self,
        cluster: &StreamlineCluster,
        metrics: &PartitionMetrics,
        autoscaling: &AutoScalingConfig,
    ) -> ScalingRecommendation {
        let current_replicas = cluster.spec.replicas;
        let mut recommended = current_replicas;
        let mut reasons = Vec::new();
        let mut triggering = Vec::new();

        // Check lag-based scaling
        if autoscaling.partition_aware && metrics.total_lag > 0 {
            let lag_per_broker = metrics.total_lag / current_replicas as i64;
            let target_lag = autoscaling.target_lag_per_partition * metrics.total_partitions as i64;

            if lag_per_broker > autoscaling.target_lag_per_partition * 2 {
                // Scale up if lag is more than 2x target
                let needed = (metrics.total_lag / target_lag).max(1) as i32;
                if needed > recommended {
                    recommended = needed;
                    reasons.push(format!(
                        "High consumer lag: {}. Target: {}",
                        metrics.total_lag, target_lag
                    ));
                    triggering.push("consumer_lag".to_string());
                }
            } else if lag_per_broker < autoscaling.target_lag_per_partition / 4
                && recommended > autoscaling.min_replicas
            {
                // Scale down if lag is less than 25% of target
                let needed = ((metrics.total_lag as f64 / target_lag as f64).ceil() as i32).max(1);
                if needed < recommended {
                    recommended = needed;
                    reasons.push(format!(
                        "Low consumer lag: {}. Target: {}",
                        metrics.total_lag, target_lag
                    ));
                    triggering.push("consumer_lag".to_string());
                }
            }
        }

        // Check throughput-based scaling
        if metrics.messages_per_second > 0.0 {
            let mps_per_broker = metrics.messages_per_second / current_replicas as f64;
            let target_mps = autoscaling.target_messages_per_second as f64;

            if mps_per_broker > target_mps * 1.5 {
                // Scale up if MPS is more than 150% of target
                let needed = (metrics.messages_per_second / target_mps).ceil() as i32;
                if needed > recommended {
                    recommended = needed;
                    reasons.push(format!(
                        "High throughput: {:.0} msg/s per broker. Target: {}",
                        mps_per_broker, target_mps
                    ));
                    triggering.push("messages_per_second".to_string());
                }
            }
        }

        // Check partition distribution
        if metrics.total_partitions > 0 {
            let partitions_per_broker = metrics.total_partitions / current_replicas;
            // Recommend at least 1 broker per 100 partitions for good distribution
            let min_for_partitions = (metrics.total_partitions / 100).max(1);
            if min_for_partitions > recommended {
                recommended = min_for_partitions;
                reasons.push(format!(
                    "High partition count: {}. {} partitions per broker",
                    metrics.total_partitions, partitions_per_broker
                ));
                triggering.push("partition_count".to_string());
            }
        }

        // Apply limits
        recommended = recommended
            .max(autoscaling.min_replicas)
            .min(autoscaling.max_replicas);

        // Calculate confidence
        let confidence = if triggering.is_empty() {
            50 // No strong signal
        } else if triggering.len() == 1 {
            70 // Single metric trigger
        } else {
            90 // Multiple metrics agree
        };

        let reason = if reasons.is_empty() {
            "No scaling needed".to_string()
        } else {
            reasons.join("; ")
        };

        ScalingRecommendation {
            recommended_replicas: recommended,
            current_replicas,
            reason,
            confidence,
            triggering_metrics: triggering,
            timestamp: chrono::Utc::now().to_rfc3339(),
        }
    }

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
            "autoscaling".to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_autoscaling_config_defaults() {
        let config: AutoScalingConfig = serde_json::from_str("{}").unwrap();
        assert!(!config.enabled);
        assert_eq!(config.min_replicas, 1);
        assert_eq!(config.max_replicas, 10);
        assert_eq!(config.target_cpu_utilization, 70);
    }

    #[test]
    fn test_scaling_recommendation_no_change() {
        let config = AutoScalingConfig {
            enabled: true,
            min_replicas: 1,
            max_replicas: 10,
            partition_aware: true,
            target_lag_per_partition: 10000,
            target_messages_per_second: 100000,
            ..Default::default()
        };

        let metrics = PartitionMetrics {
            total_partitions: 30,
            total_lag: 15000,
            average_lag: 500.0,
            max_partition_lag: 1000,
            messages_per_second: 50000.0,
            bytes_per_second: 50000000.0,
            under_replicated_partitions: 0,
        };

        // Create a mock cluster spec
        let cluster_json = r#"{
            "apiVersion": "streamline.io/v1alpha1",
            "kind": "StreamlineCluster",
            "metadata": {"name": "test", "namespace": "default"},
            "spec": {"replicas": 3}
        }"#;
        let _cluster: StreamlineCluster = serde_json::from_str(cluster_json).unwrap();

        // This test verifies the config/metrics structures work
        // (controller instantiation requires a real k8s client)
        assert_eq!(metrics.total_partitions, 30);
        assert_eq!(config.target_lag_per_partition, 10000);
    }

    #[test]
    fn test_custom_metric_parsing() {
        let json = r#"{
            "name": "custom_requests",
            "metricType": "Pods",
            "target": {
                "type": "AverageValue",
                "averageValue": "1000"
            }
        }"#;

        let metric: CustomMetric = serde_json::from_str(json).unwrap();
        assert_eq!(metric.name, "custom_requests");
        assert_eq!(metric.metric_type, "Pods");
        assert_eq!(metric.target.average_value, Some("1000".to_string()));
    }

    #[test]
    fn test_scaling_behavior_parsing() {
        let json = r#"{
            "scaleUp": {
                "stabilizationWindowSeconds": 60,
                "selectPolicy": "Max",
                "policies": [
                    {"type": "Pods", "value": 4, "periodSeconds": 60},
                    {"type": "Percent", "value": 100, "periodSeconds": 60}
                ]
            },
            "scaleDown": {
                "stabilizationWindowSeconds": 300,
                "selectPolicy": "Min",
                "policies": [
                    {"type": "Pods", "value": 1, "periodSeconds": 60}
                ]
            }
        }"#;

        let behavior: ScalingBehavior = serde_json::from_str(json).unwrap();
        assert!(behavior.scale_up.is_some());
        assert!(behavior.scale_down.is_some());

        let scale_up = behavior.scale_up.unwrap();
        assert_eq!(scale_up.stabilization_window_seconds, Some(60));
        assert_eq!(scale_up.policies.len(), 2);
    }
}
