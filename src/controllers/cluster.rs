//! Cluster Controller
//!
//! Reconciles StreamlineCluster custom resources to manage StatefulSets,
//! Services, and ConfigMaps for Streamline clusters.

use crate::conditions::{
    build_condition, set_condition, CLUSTER_CONDITION_AVAILABLE, CLUSTER_CONDITION_DEGRADED,
    CLUSTER_CONDITION_PROGRESSING, CLUSTER_CONDITION_READY, CLUSTER_FINALIZER, CONDITION_FALSE,
    CONDITION_TRUE,
};
use crate::crd::{ClusterPhase, ClusterStatus, ClusterStorage, StreamlineCluster};
use crate::error::{OperatorError, Result};
use chrono::Utc;
use futures::StreamExt;
use k8s_openapi::api::apps::v1::{
    RollingUpdateStatefulSetStrategy, StatefulSet, StatefulSetSpec,
    StatefulSetUpdateStrategy,
};
use k8s_openapi::api::core::v1::{
    ConfigMap, Container, ContainerPort, EnvVar, PersistentVolumeClaim, PersistentVolumeClaimSpec,
    Pod, PodSpec, PodTemplateSpec, Probe, ResourceRequirements, Service, ServicePort, ServiceSpec,
    VolumeMount, VolumeResourceRequirements,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta, OwnerReference};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::api::{Api, ListParams, Patch, PatchParams, PostParams};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher::Config;
use kube::{Client, Resource, ResourceExt};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info};

/// Context for the cluster controller
pub struct ClusterController {
    client: Client,
}

impl ClusterController {
    /// Create a new cluster controller
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    /// Run the cluster controller
    pub async fn run(self: Arc<Self>) -> Result<()> {
        let clusters: Api<StreamlineCluster> = Api::all(self.client.clone());

        info!("Starting StreamlineCluster controller");

        Controller::new(clusters, Config::default())
            .shutdown_on_signal()
            .run(
                |cluster, ctx| async move { ctx.reconcile(cluster).await },
                |_cluster, error, _ctx| {
                    error!("Reconciliation error: {:?}", error);
                    Action::requeue(Duration::from_secs(30))
                },
                Arc::clone(&self),
            )
            .for_each(|result| async move {
                match result {
                    Ok((obj, _action)) => {
                        info!("Reconciled cluster: {}", obj.name);
                    }
                    Err(e) => {
                        error!("Reconciliation failed: {:?}", e);
                    }
                }
            })
            .await;

        Ok(())
    }

    /// Reconcile a StreamlineCluster
    async fn reconcile(
        &self,
        cluster: Arc<StreamlineCluster>,
    ) -> std::result::Result<Action, OperatorError> {
        let name = cluster.name_any();
        let namespace = cluster.namespace().unwrap_or_else(|| "default".to_string());

        info!("Reconciling StreamlineCluster {}/{}", namespace, name);

        // Handle deletion with finalizer
        if cluster.metadata.deletion_timestamp.is_some() {
            return self.handle_deletion(&cluster, &namespace).await;
        }

        // Ensure finalizer is set
        self.ensure_finalizer(&cluster, &namespace).await?;

        // Create/update ConfigMap
        self.reconcile_configmap(&cluster, &namespace).await?;

        // Create/update headless Service for StatefulSet
        self.reconcile_headless_service(&cluster, &namespace)
            .await?;

        // Create/update client-facing Service
        self.reconcile_client_service(&cluster, &namespace).await?;

        // Create/update StatefulSet
        self.reconcile_statefulset(&cluster, &namespace).await?;

        // Update status
        self.update_status(&cluster, &namespace).await?;

        Ok(Action::requeue(Duration::from_secs(60)))
    }

    /// Ensure the finalizer is present on the resource
    async fn ensure_finalizer(
        &self,
        cluster: &StreamlineCluster,
        namespace: &str,
    ) -> Result<()> {
        let finalizers = cluster.metadata.finalizers.as_deref().unwrap_or_default();
        if finalizers.contains(&CLUSTER_FINALIZER.to_string()) {
            return Ok(());
        }

        let clusters: Api<StreamlineCluster> = Api::namespaced(self.client.clone(), namespace);
        let patch = serde_json::json!({
            "metadata": {
                "finalizers": [CLUSTER_FINALIZER]
            }
        });
        clusters
            .patch(
                &cluster.name_any(),
                &PatchParams::apply("streamline-operator").force(),
                &Patch::Apply(&patch),
            )
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        Ok(())
    }

    /// Handle deletion: clean up PVCs, services, configmaps, then remove finalizer
    async fn handle_deletion(
        &self,
        cluster: &StreamlineCluster,
        namespace: &str,
    ) -> std::result::Result<Action, OperatorError> {
        let name = cluster.name_any();
        info!("Handling deletion of StreamlineCluster {}/{}", namespace, name);

        // Clean up PVCs created by the StatefulSet
        let pvcs: Api<PersistentVolumeClaim> = Api::namespaced(self.client.clone(), namespace);
        let pvc_list = pvcs
            .list(&ListParams::default().labels(&format!("app.kubernetes.io/instance={}", name)))
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        for pvc in &pvc_list.items {
            if let Some(pvc_name) = &pvc.metadata.name {
                info!("Cleaning up PVC {}/{}", namespace, pvc_name);
                let _ = pvcs.delete(pvc_name, &Default::default()).await;
            }
        }

        // Remove finalizer
        let clusters: Api<StreamlineCluster> = Api::namespaced(self.client.clone(), namespace);
        let finalizers: Vec<String> = cluster
            .metadata
            .finalizers
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|f| f.as_str() != CLUSTER_FINALIZER)
            .cloned()
            .collect();

        let patch = serde_json::json!({
            "metadata": {
                "finalizers": finalizers
            }
        });
        clusters
            .patch(
                &name,
                &PatchParams::default(),
                &Patch::Merge(&patch),
            )
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        info!("Finalizer removed for StreamlineCluster {}/{}", namespace, name);
        Ok(Action::await_change())
    }

    /// Reconcile the ConfigMap for cluster configuration
    async fn reconcile_configmap(
        &self,
        cluster: &StreamlineCluster,
        namespace: &str,
    ) -> Result<()> {
        let name = format!("{}-config", cluster.name_any());
        let configmaps: Api<ConfigMap> = Api::namespaced(self.client.clone(), namespace);

        let labels = self.common_labels(cluster);
        let owner_ref = self.owner_reference(cluster);

        let mut config_data = BTreeMap::new();

        // Build streamline configuration
        let mut config_yaml = format!(
            r#"# Streamline configuration
kafka_port: {}
http_port: {}
raft_port: {}
log_level: {}
metrics_enabled: {}
"#,
            cluster.spec.kafka_port,
            cluster.spec.http_port,
            cluster.spec.raft_port,
            cluster.spec.log_level,
            cluster.spec.metrics_enabled
        );

        if let Some(tls) = &cluster.spec.tls {
            if tls.enabled {
                config_yaml.push_str(&format!(
                    r#"
tls:
  enabled: true
  cert_file: /etc/streamline/tls/tls.crt
  key_file: /etc/streamline/tls/tls.key
  mtls_enabled: {}
"#,
                    tls.mtls_enabled
                ));
            }
        }

        config_data.insert("streamline.yaml".to_string(), config_yaml);

        let configmap = ConfigMap {
            metadata: ObjectMeta {
                name: Some(name.clone()),
                namespace: Some(namespace.to_string()),
                labels: Some(labels),
                owner_references: Some(vec![owner_ref]),
                ..Default::default()
            },
            data: Some(config_data),
            ..Default::default()
        };

        match configmaps.get(&name).await {
            Ok(_existing) => {
                configmaps
                    .patch(
                        &name,
                        &PatchParams::apply("streamline-operator"),
                        &Patch::Apply(&configmap),
                    )
                    .await
                    .map_err(|e| OperatorError::KubeApi(e.to_string()))?;
            }
            Err(_) => {
                configmaps
                    .create(&PostParams::default(), &configmap)
                    .await
                    .map_err(|e| OperatorError::KubeApi(e.to_string()))?;
            }
        }

        Ok(())
    }

    /// Reconcile the headless service for StatefulSet DNS
    async fn reconcile_headless_service(
        &self,
        cluster: &StreamlineCluster,
        namespace: &str,
    ) -> Result<()> {
        let name = format!("{}-headless", cluster.name_any());
        let services: Api<Service> = Api::namespaced(self.client.clone(), namespace);

        let labels = self.common_labels(cluster);
        let selector = self.pod_selector(cluster);
        let owner_ref = self.owner_reference(cluster);

        let service = Service {
            metadata: ObjectMeta {
                name: Some(name.clone()),
                namespace: Some(namespace.to_string()),
                labels: Some(labels),
                owner_references: Some(vec![owner_ref]),
                ..Default::default()
            },
            spec: Some(ServiceSpec {
                cluster_ip: Some("None".to_string()),
                selector: Some(selector),
                ports: Some(vec![
                    ServicePort {
                        name: Some("kafka".to_string()),
                        port: cluster.spec.kafka_port,
                        ..Default::default()
                    },
                    ServicePort {
                        name: Some("http".to_string()),
                        port: cluster.spec.http_port,
                        ..Default::default()
                    },
                    ServicePort {
                        name: Some("raft".to_string()),
                        port: cluster.spec.raft_port,
                        ..Default::default()
                    },
                ]),
                publish_not_ready_addresses: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        };

        self.apply_service(&services, &name, service).await
    }

    /// Reconcile the client-facing service
    async fn reconcile_client_service(
        &self,
        cluster: &StreamlineCluster,
        namespace: &str,
    ) -> Result<()> {
        let name = cluster.name_any();
        let services: Api<Service> = Api::namespaced(self.client.clone(), namespace);

        let labels = self.common_labels(cluster);
        let selector = self.pod_selector(cluster);
        let owner_ref = self.owner_reference(cluster);

        let service = Service {
            metadata: ObjectMeta {
                name: Some(name.clone()),
                namespace: Some(namespace.to_string()),
                labels: Some(labels),
                owner_references: Some(vec![owner_ref]),
                ..Default::default()
            },
            spec: Some(ServiceSpec {
                selector: Some(selector),
                ports: Some(vec![
                    ServicePort {
                        name: Some("kafka".to_string()),
                        port: cluster.spec.kafka_port,
                        ..Default::default()
                    },
                    ServicePort {
                        name: Some("http".to_string()),
                        port: cluster.spec.http_port,
                        ..Default::default()
                    },
                ]),
                ..Default::default()
            }),
            ..Default::default()
        };

        self.apply_service(&services, &name, service).await
    }

    async fn apply_service(
        &self,
        services: &Api<Service>,
        name: &str,
        service: Service,
    ) -> Result<()> {
        match services.get(name).await {
            Ok(_existing) => {
                services
                    .patch(
                        name,
                        &PatchParams::apply("streamline-operator"),
                        &Patch::Apply(&service),
                    )
                    .await
                    .map_err(|e| OperatorError::KubeApi(e.to_string()))?;
            }
            Err(_) => {
                services
                    .create(&PostParams::default(), &service)
                    .await
                    .map_err(|e| OperatorError::KubeApi(e.to_string()))?;
            }
        }
        Ok(())
    }

    /// Reconcile the StatefulSet for Streamline brokers
    async fn reconcile_statefulset(
        &self,
        cluster: &StreamlineCluster,
        namespace: &str,
    ) -> Result<()> {
        let name = cluster.name_any();
        let statefulsets: Api<StatefulSet> = Api::namespaced(self.client.clone(), namespace);

        let labels = self.common_labels(cluster);
        let selector = self.pod_selector(cluster);
        let owner_ref = self.owner_reference(cluster);

        // Build environment variables
        let mut env_vars = vec![
            EnvVar {
                name: "STREAMLINE_DATA_DIR".to_string(),
                value: Some("/data".to_string()),
                ..Default::default()
            },
            EnvVar {
                name: "STREAMLINE_LISTEN_ADDR".to_string(),
                value: Some(format!("0.0.0.0:{}", cluster.spec.kafka_port)),
                ..Default::default()
            },
            EnvVar {
                name: "STREAMLINE_HTTP_ADDR".to_string(),
                value: Some(format!("0.0.0.0:{}", cluster.spec.http_port)),
                ..Default::default()
            },
            EnvVar {
                name: "STREAMLINE_LOG_LEVEL".to_string(),
                value: Some(cluster.spec.log_level.clone()),
                ..Default::default()
            },
            EnvVar {
                name: "POD_NAME".to_string(),
                value_from: Some(k8s_openapi::api::core::v1::EnvVarSource {
                    field_ref: Some(k8s_openapi::api::core::v1::ObjectFieldSelector {
                        field_path: "metadata.name".to_string(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
            EnvVar {
                name: "POD_NAMESPACE".to_string(),
                value_from: Some(k8s_openapi::api::core::v1::EnvVarSource {
                    field_ref: Some(k8s_openapi::api::core::v1::ObjectFieldSelector {
                        field_path: "metadata.namespace".to_string(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ];

        // Add custom env vars from spec
        for env in &cluster.spec.env {
            env_vars.push(EnvVar {
                name: env.name.clone(),
                value: env.value.clone(),
                ..Default::default()
            });
        }

        // Build resource requirements
        let resources = self.build_resource_requirements(&cluster.spec.resources);

        // Build volume claim template
        let volume_claim_templates = self.build_volume_claim_templates(&cluster.spec.storage);

        // Build container
        let container = Container {
            name: "streamline".to_string(),
            image: Some(cluster.spec.image.clone()),
            image_pull_policy: Some(cluster.spec.image_pull_policy.clone()),
            ports: Some(vec![
                ContainerPort {
                    name: Some("kafka".to_string()),
                    container_port: cluster.spec.kafka_port,
                    ..Default::default()
                },
                ContainerPort {
                    name: Some("http".to_string()),
                    container_port: cluster.spec.http_port,
                    ..Default::default()
                },
                ContainerPort {
                    name: Some("raft".to_string()),
                    container_port: cluster.spec.raft_port,
                    ..Default::default()
                },
            ]),
            env: Some(env_vars),
            resources: Some(resources),
            volume_mounts: Some(vec![
                VolumeMount {
                    name: "data".to_string(),
                    mount_path: "/data".to_string(),
                    ..Default::default()
                },
                VolumeMount {
                    name: "config".to_string(),
                    mount_path: "/etc/streamline".to_string(),
                    ..Default::default()
                },
            ]),
            liveness_probe: Some(Probe {
                http_get: Some(k8s_openapi::api::core::v1::HTTPGetAction {
                    path: Some("/health".to_string()),
                    port: k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::Int(
                        cluster.spec.http_port,
                    ),
                    ..Default::default()
                }),
                initial_delay_seconds: Some(30),
                period_seconds: Some(10),
                timeout_seconds: Some(5),
                failure_threshold: Some(3),
                ..Default::default()
            }),
            readiness_probe: Some(Probe {
                http_get: Some(k8s_openapi::api::core::v1::HTTPGetAction {
                    path: Some("/ready".to_string()),
                    port: k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::Int(
                        cluster.spec.http_port,
                    ),
                    ..Default::default()
                }),
                initial_delay_seconds: Some(10),
                period_seconds: Some(5),
                timeout_seconds: Some(3),
                failure_threshold: Some(3),
                ..Default::default()
            }),
            startup_probe: Some(Probe {
                http_get: Some(k8s_openapi::api::core::v1::HTTPGetAction {
                    path: Some("/health".to_string()),
                    port: k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::Int(
                        cluster.spec.http_port,
                    ),
                    ..Default::default()
                }),
                initial_delay_seconds: Some(5),
                period_seconds: Some(5),
                timeout_seconds: Some(3),
                failure_threshold: Some(30),
                ..Default::default()
            }),
            ..Default::default()
        };

        // Build pod template
        let pod_template = PodTemplateSpec {
            metadata: Some(ObjectMeta {
                labels: Some(selector.clone()),
                ..Default::default()
            }),
            spec: Some(PodSpec {
                service_account_name: cluster.spec.service_account_name.clone(),
                containers: vec![container],
                node_selector: if cluster.spec.node_selector.is_empty() {
                    None
                } else {
                    Some(cluster.spec.node_selector.clone())
                },
                volumes: Some(vec![k8s_openapi::api::core::v1::Volume {
                    name: "config".to_string(),
                    config_map: Some(k8s_openapi::api::core::v1::ConfigMapVolumeSource {
                        name: format!("{}-config", cluster.name_any()),
                        ..Default::default()
                    }),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
        };

        // Build update strategy from spec
        let update_strategy = if cluster.spec.update_strategy.r#type == "OnDelete" {
            Some(StatefulSetUpdateStrategy {
                type_: Some("OnDelete".to_string()),
                ..Default::default()
            })
        } else {
            Some(StatefulSetUpdateStrategy {
                type_: Some("RollingUpdate".to_string()),
                rolling_update: cluster.spec.update_strategy.max_unavailable.map(|max| {
                    RollingUpdateStatefulSetStrategy {
                        max_unavailable: Some(IntOrString::Int(max)),
                        ..Default::default()
                    }
                }),
            })
        };

        let statefulset = StatefulSet {
            metadata: ObjectMeta {
                name: Some(name.clone()),
                namespace: Some(namespace.to_string()),
                labels: Some(labels),
                owner_references: Some(vec![owner_ref]),
                ..Default::default()
            },
            spec: Some(StatefulSetSpec {
                replicas: Some(cluster.spec.replicas),
                selector: LabelSelector {
                    match_labels: Some(selector),
                    ..Default::default()
                },
                service_name: format!("{}-headless", name),
                template: pod_template,
                volume_claim_templates: Some(volume_claim_templates),
                pod_management_policy: Some("Parallel".to_string()),
                update_strategy,
                ..Default::default()
            }),
            ..Default::default()
        };

        match statefulsets.get(&name).await {
            Ok(_existing) => {
                statefulsets
                    .patch(
                        &name,
                        &PatchParams::apply("streamline-operator"),
                        &Patch::Apply(&statefulset),
                    )
                    .await
                    .map_err(|e| OperatorError::KubeApi(e.to_string()))?;
            }
            Err(_) => {
                statefulsets
                    .create(&PostParams::default(), &statefulset)
                    .await
                    .map_err(|e| OperatorError::KubeApi(e.to_string()))?;
            }
        }

        Ok(())
    }

    /// Update the cluster status with Kubernetes-standard conditions
    async fn update_status(&self, cluster: &StreamlineCluster, namespace: &str) -> Result<()> {
        let name = cluster.name_any();
        let clusters: Api<StreamlineCluster> = Api::namespaced(self.client.clone(), namespace);
        let pods: Api<Pod> = Api::namespaced(self.client.clone(), namespace);

        // Count ready pods
        let pod_list = pods
            .list(&ListParams::default().labels(&format!("app.kubernetes.io/instance={}", name)))
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        let ready_count = pod_list
            .items
            .iter()
            .filter(|pod| {
                pod.status.as_ref().is_some_and(|s| {
                    s.conditions.as_ref().is_some_and(|conditions| {
                        conditions
                            .iter()
                            .any(|c| c.type_ == "Ready" && c.status == "True")
                    })
                })
            })
            .count() as i32;

        let desired = cluster.spec.replicas;

        let phase = if ready_count == desired {
            ClusterPhase::Running
        } else if ready_count > 0 {
            ClusterPhase::Scaling
        } else {
            ClusterPhase::Pending
        };

        // Build broker endpoints
        let broker_endpoints: Vec<String> = (0..desired)
            .map(|i| {
                format!(
                    "{}-{}.{}-headless.{}.svc:{}",
                    name, i, name, namespace, cluster.spec.kafka_port
                )
            })
            .collect();

        // Build Kubernetes-standard conditions
        let mut cond_fields = Vec::new();

        // Ready condition
        let (ready_status, ready_reason, ready_msg) = if ready_count == desired {
            (CONDITION_TRUE, "AllBrokersReady", format!("{}/{} brokers ready", ready_count, desired))
        } else {
            (CONDITION_FALSE, "BrokersNotReady", format!("{}/{} brokers ready", ready_count, desired))
        };
        set_condition(&mut cond_fields, build_condition(CLUSTER_CONDITION_READY, ready_status, ready_reason, &ready_msg));

        // Available condition — at least one broker is ready
        let (avail_status, avail_reason, avail_msg) = if ready_count > 0 {
            (CONDITION_TRUE, "MinimumAvailable", format!("{} broker(s) available", ready_count))
        } else {
            (CONDITION_FALSE, "NoBrokersAvailable", "No brokers are available".to_string())
        };
        set_condition(&mut cond_fields, build_condition(CLUSTER_CONDITION_AVAILABLE, avail_status, avail_reason, &avail_msg));

        // Progressing condition — rolling out or scaling
        let (prog_status, prog_reason, prog_msg) = if ready_count < desired {
            (CONDITION_TRUE, "ScalingUp", format!("Scaling from {} to {} replicas", ready_count, desired))
        } else {
            (CONDITION_FALSE, "UpToDate", "All replicas are up to date".to_string())
        };
        set_condition(&mut cond_fields, build_condition(CLUSTER_CONDITION_PROGRESSING, prog_status, prog_reason, &prog_msg));

        // Degraded condition — some brokers are down
        let (deg_status, deg_reason, deg_msg) = if ready_count > 0 && ready_count < desired {
            (CONDITION_TRUE, "PartiallyReady", format!("Only {}/{} brokers ready", ready_count, desired))
        } else {
            (CONDITION_FALSE, "Healthy", "Cluster is healthy".to_string())
        };
        set_condition(&mut cond_fields, build_condition(CLUSTER_CONDITION_DEGRADED, deg_status, deg_reason, &deg_msg));

        let conditions = cond_fields
            .into_iter()
            .map(|c| c.into_cluster_condition())
            .collect();

        let status = ClusterStatus {
            phase,
            ready_replicas: ready_count,
            replicas: desired,
            leader_id: None,
            broker_endpoints,
            conditions,
            observed_generation: cluster.metadata.generation,
            last_updated: Some(Utc::now().to_rfc3339()),
        };

        let patch = serde_json::json!({
            "status": status
        });

        clusters
            .patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        Ok(())
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
        labels
    }

    fn pod_selector(&self, cluster: &StreamlineCluster) -> BTreeMap<String, String> {
        let mut labels = BTreeMap::new();
        labels.insert(
            "app.kubernetes.io/name".to_string(),
            "streamline".to_string(),
        );
        labels.insert("app.kubernetes.io/instance".to_string(), cluster.name_any());
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

    fn build_resource_requirements(
        &self,
        spec: &crate::crd::ResourceRequirements,
    ) -> ResourceRequirements {
        let mut limits = BTreeMap::new();
        let mut requests = BTreeMap::new();

        if let Some(cpu) = &spec.limits.cpu {
            limits.insert("cpu".to_string(), Quantity(cpu.clone()));
        }
        if let Some(memory) = &spec.limits.memory {
            limits.insert("memory".to_string(), Quantity(memory.clone()));
        }
        if let Some(cpu) = &spec.requests.cpu {
            requests.insert("cpu".to_string(), Quantity(cpu.clone()));
        }
        if let Some(memory) = &spec.requests.memory {
            requests.insert("memory".to_string(), Quantity(memory.clone()));
        }

        ResourceRequirements {
            limits: if limits.is_empty() {
                None
            } else {
                Some(limits)
            },
            requests: if requests.is_empty() {
                None
            } else {
                Some(requests)
            },
            ..Default::default()
        }
    }

    fn build_volume_claim_templates(&self, storage: &ClusterStorage) -> Vec<PersistentVolumeClaim> {
        vec![PersistentVolumeClaim {
            metadata: ObjectMeta {
                name: Some("data".to_string()),
                ..Default::default()
            },
            spec: Some(PersistentVolumeClaimSpec {
                access_modes: Some(storage.access_modes.clone()),
                storage_class_name: storage.storage_class_name.clone(),
                resources: Some(VolumeResourceRequirements {
                    requests: Some({
                        let mut reqs = BTreeMap::new();
                        reqs.insert("storage".to_string(), Quantity(storage.size.clone()));
                        reqs
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        }]
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_cluster_controller() {
        // Controller tests require k8s cluster
    }
}
