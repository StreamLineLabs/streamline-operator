//! Cluster Controller
//!
//! Reconciles StreamlineCluster custom resources to manage StatefulSets,
//! Services, and ConfigMaps for Streamline clusters.

use crate::conditions::{
    build_condition, set_condition, CLUSTER_CONDITION_AVAILABLE, CLUSTER_CONDITION_DEGRADED,
    CLUSTER_CONDITION_PROGRESSING, CLUSTER_CONDITION_READY, CLUSTER_FINALIZER, CONDITION_FALSE,
    CONDITION_TRUE,
};
use crate::controllers::{error_policy_backoff, WatchScope};
use crate::crd::{ClusterPhase, ClusterStatus, ClusterStorage, EnvVarRef, StreamlineCluster};
use crate::error::{OperatorError, Result};
use chrono::Utc;
use futures::StreamExt;
use k8s_openapi::api::apps::v1::{
    RollingUpdateStatefulSetStrategy, StatefulSet, StatefulSetSpec, StatefulSetUpdateStrategy,
};
use k8s_openapi::api::core::v1::{
    ConfigMap, Container, ContainerPort, EnvVar, PersistentVolumeClaim, PersistentVolumeClaimSpec,
    Pod, PodSecurityContext, PodSpec, PodTemplateSpec, Probe, ResourceRequirements, Service,
    ServicePort, ServiceSpec, VolumeMount, VolumeResourceRequirements,
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
    scope: WatchScope,
}

/// File name of the rendered broker configuration inside the ConfigMap and the
/// mounted config directory.
///
/// The Streamline server reads a TOML configuration file whose path is passed
/// through the `STREAMLINE_CONFIG` environment variable; the previous YAML
/// document was never read by the broker.
const CONFIG_FILE_NAME: &str = "streamline.toml";

/// Directory the rendered configuration is mounted at inside the broker pod.
const CONFIG_MOUNT_PATH: &str = "/etc/streamline";

/// Absolute path of the rendered configuration inside the broker pod.
const CONFIG_FILE_PATH: &str = "/etc/streamline/streamline.toml";

/// Directory the server TLS keypair is mounted at inside the broker pod.
///
/// This is deliberately a **sibling** of [`CONFIG_MOUNT_PATH`], not a
/// subdirectory of it. Nesting a Secret volume under the read-only ConfigMap
/// mount (the old `/etc/streamline/tls`) is not a supported kubelet layout:
/// the ConfigMap volume is projected read-only, so mounting into a path that
/// lives inside it is fragile at best and, on a container runtime that
/// remounts the parent, hides the certificates entirely — leaving the broker
/// pointed at a `cert` path that does not exist.
const TLS_MOUNT_PATH: &str = "/etc/streamline-tls";

/// Directory the client CA bundle (mTLS) is mounted at inside the broker pod.
///
/// A sibling of [`CONFIG_MOUNT_PATH`] for the same reason as
/// [`TLS_MOUNT_PATH`].
const TLS_CA_MOUNT_PATH: &str = "/etc/streamline-tls-ca";

/// Volume name for the server TLS keypair.
const TLS_VOLUME_NAME: &str = "tls";

/// Volume name for the client CA bundle.
const TLS_CA_VOLUME_NAME: &str = "tls-ca";

/// Render the broker configuration file contents for a cluster.
///
/// The output is TOML because that is what the Streamline server parses from
/// `$STREAMLINE_CONFIG`. The shape mirrors the server's `ConfigFile` sections
/// exactly:
///
/// * `[server]` — `listen_addr`, `http_addr`, `data_dir`, `log_level`
/// * `[tls]` — `enabled`, `cert`, `key`, `require_client_cert`, `ca_cert`
///
/// Only keys the server actually reads are emitted, and they are emitted under
/// the section the server parses them from. In particular there is no
/// top-level `raft_addr` or `metrics_enabled`: the server's configuration file
/// defines neither, so rendering them advertised configuration the broker
/// silently ignored. The raft port is still published on the Service and Pod
/// ports, and the broker serves `/metrics` on its HTTP port unconditionally.
///
/// Environment variables (`STREAMLINE_*`) continue to be set as well, so either
/// mechanism yields the same values.
fn render_config(cluster: &StreamlineCluster) -> String {
    let spec = &cluster.spec;
    let mut config = String::new();
    config.push_str("# Managed by streamline-operator. Do not edit.\n");
    config.push_str(&format!(
        "# Rendered from StreamlineCluster/{}\n\n",
        cluster.name_any()
    ));
    config.push_str("[server]\n");
    config.push_str(&format!("listen_addr = \"0.0.0.0:{}\"\n", spec.kafka_port));
    config.push_str(&format!("http_addr = \"0.0.0.0:{}\"\n", spec.http_port));
    config.push_str("data_dir = \"/data\"\n");
    config.push_str(&format!("log_level = \"{}\"\n", spec.log_level));

    if let Some(tls) = &spec.tls {
        // Unsupported combinations are rejected by `ClusterSpec::validate()`
        // before rendering, so anything reaching here is mountable.
        if tls.enabled {
            config.push_str("\n[tls]\n");
            config.push_str("enabled = true\n");
            config.push_str(&format!("cert = \"{TLS_MOUNT_PATH}/tls.crt\"\n"));
            config.push_str(&format!("key = \"{TLS_MOUNT_PATH}/tls.key\"\n"));
            if tls.mtls_enabled {
                config.push_str("require_client_cert = true\n");
                config.push_str(&format!("ca_cert = \"{TLS_CA_MOUNT_PATH}/ca.crt\"\n"));
            }
        }
    }

    config
}

impl ClusterController {
    /// Create a new cluster controller watching `scope`.
    pub fn new(client: Client, scope: WatchScope) -> Self {
        Self { client, scope }
    }

    /// Run the cluster controller
    pub async fn run(self: Arc<Self>) -> Result<()> {
        let clusters: Api<StreamlineCluster> = self.scope.api(self.client.clone());

        info!(
            "Starting StreamlineCluster controller (watching {})",
            self.scope.describe()
        );

        Controller::new(clusters, Config::default())
            .shutdown_on_signal()
            .run(
                |cluster, ctx| async move { ctx.reconcile(cluster).await },
                |_cluster, error, _ctx| {
                    error!("Reconciliation error: {:?}", error);
                    crate::metrics::get().inc_error("cluster");
                    error_policy_backoff(_cluster, error, _ctx)
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
        crate::metrics::get().inc_reconcile("cluster");
        let _timer = crate::metrics::get().start_timer();
        let name = cluster.name_any();
        let namespace = cluster.namespace().unwrap_or_else(|| "default".to_string());

        info!("Reconciling StreamlineCluster {}/{}", namespace, name);

        // Handle deletion with finalizer
        if cluster.metadata.deletion_timestamp.is_some() {
            return self.handle_deletion(&cluster, &namespace).await;
        }

        // Ensure finalizer is set
        self.ensure_finalizer(&cluster, &namespace).await?;

        let validation = cluster.spec.validate();
        let hpa_config = Self::desired_hpa_config(&cluster.spec, validation.is_ok());

        // Tear down an HPA before anything else when the operator will not
        // autoscale this cluster — whether autoscaling is absent, explicitly
        // disabled, or rejected. An earlier version of the operator created an
        // HPA for `autoscaling.enabled: true`; that HPA outlives the upgrade
        // (and outlives deleting the field) and keeps scaling the StatefulSet
        // to multiple independent brokers. Running this on every reconcile,
        // and before the validation early return below, is what makes the
        // rejection real rather than advisory. A delete of an absent HPA is a
        // 404 the autoscaling controller treats as success.
        if !hpa_config.enabled {
            self.reconcile_autoscaling(&cluster, &namespace, &hpa_config)
                .await?;
        }

        // Reject specs the operator cannot faithfully render (including
        // unsupported TLS combinations) instead of creating workloads that
        // silently ignore them.
        if let Err(errors) = validation {
            let message = errors.join("; ");
            error!(
                "StreamlineCluster {}/{} has an unsupported spec: {}",
                namespace, name, message
            );
            self.update_status_invalid(&cluster, &namespace, &message)
                .await?;
            crate::metrics::get().inc_error("cluster");
            // Requeue slowly: only a spec change can fix this.
            return Ok(Action::requeue(Duration::from_secs(300)));
        }

        // Create/update ConfigMap
        self.reconcile_configmap(&cluster, &namespace).await?;

        // Create/update headless Service for StatefulSet
        self.reconcile_headless_service(&cluster, &namespace)
            .await?;

        // Create/update client-facing Service
        self.reconcile_client_service(&cluster, &namespace).await?;

        // Create/update StatefulSet
        self.reconcile_statefulset(&cluster, &namespace).await?;

        // Create/update HPA if autoscaling is enabled. Disabled configurations
        // were already reconciled (and their stale HPA deleted) before the
        // validation gate above.
        if hpa_config.enabled {
            self.reconcile_autoscaling(&cluster, &namespace, &hpa_config)
                .await?;
            info!("HPA reconciled for cluster {}/{}", namespace, name);
        }

        // Update status
        self.update_status(&cluster, &namespace).await?;

        crate::metrics::get().inc_success();
        Ok(Action::requeue(Duration::from_secs(60)))
    }

    /// Ensure the finalizer is present on the resource
    async fn ensure_finalizer(&self, cluster: &StreamlineCluster, namespace: &str) -> Result<()> {
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
        info!(
            "Handling deletion of StreamlineCluster {}/{}",
            namespace, name
        );

        // Clean up PVCs created by the StatefulSet
        let pvcs: Api<PersistentVolumeClaim> = Api::namespaced(self.client.clone(), namespace);
        let pvc_list = pvcs
            .list(&ListParams::default().labels(&format!("app.kubernetes.io/instance={name}")))
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
            .patch(&name, &PatchParams::default(), &Patch::Merge(&patch))
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        info!(
            "Finalizer removed for StreamlineCluster {}/{}",
            namespace, name
        );
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

        let configmap = Self::build_configmap(cluster, namespace);

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

    /// Build the ConfigMap holding the rendered broker configuration.
    ///
    /// Pure function of the spec so the rendered output can be asserted in unit
    /// tests without a Kubernetes API server.
    fn build_configmap(cluster: &StreamlineCluster, namespace: &str) -> ConfigMap {
        let mut config_data = BTreeMap::new();
        config_data.insert(CONFIG_FILE_NAME.to_string(), render_config(cluster));

        ConfigMap {
            metadata: ObjectMeta {
                name: Some(format!("{}-config", cluster.name_any())),
                namespace: Some(namespace.to_string()),
                labels: Some(Self::common_labels(cluster)),
                owner_references: Some(vec![Self::owner_reference(cluster)]),
                ..Default::default()
            },
            data: Some(config_data),
            ..Default::default()
        }
    }

    /// Reconcile the headless service for StatefulSet DNS
    async fn reconcile_headless_service(
        &self,
        cluster: &StreamlineCluster,
        namespace: &str,
    ) -> Result<()> {
        let name = format!("{}-headless", cluster.name_any());
        let services: Api<Service> = Api::namespaced(self.client.clone(), namespace);

        let labels = Self::common_labels(cluster);
        let selector = Self::pod_selector(cluster);
        let owner_ref = Self::owner_reference(cluster);

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

        let labels = Self::common_labels(cluster);
        let selector = Self::pod_selector(cluster);
        let owner_ref = Self::owner_reference(cluster);

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

        let statefulset = Self::build_statefulset(cluster, namespace);

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

    /// Build the desired StatefulSet for a cluster.
    ///
    /// Pure function of the spec: every rendering decision (image, probes,
    /// mounts, environment) is asserted by unit tests without a Kubernetes API
    /// server.
    fn build_statefulset(cluster: &StreamlineCluster, namespace: &str) -> StatefulSet {
        let name = cluster.name_any();
        let labels = Self::common_labels(cluster);
        let selector = Self::pod_selector(cluster);
        let owner_ref = Self::owner_reference(cluster);

        // Build environment variables
        let mut env_vars = vec![
            EnvVar {
                // Path of the TOML configuration rendered into the ConfigMap.
                name: "STREAMLINE_CONFIG".to_string(),
                value: Some(CONFIG_FILE_PATH.to_string()),
                ..Default::default()
            },
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

        // Add custom env vars from spec.
        //
        // `valueFrom` is mapped onto the container's own `EnvVarSource`, so the
        // kubelet resolves the reference at pod start and the operator needs no
        // read access to Secrets or ConfigMaps. Entries the CRD cannot map
        // exactly are rejected by `ClusterSpec::validate` before reconcile
        // reaches this point; skipping them here too means an unmappable entry
        // can never be rendered as an empty variable, whatever calls this.
        for env in &cluster.spec.env {
            match env.resolve_source() {
                Ok(None) => env_vars.push(EnvVar {
                    name: env.name.clone(),
                    value: env.value.clone(),
                    ..Default::default()
                }),
                Ok(Some(source)) => env_vars.push(EnvVar {
                    name: env.name.clone(),
                    value_from: Some(Self::build_env_var_source(source)),
                    ..Default::default()
                }),
                Err(reason) => {
                    error!(
                        "StreamlineCluster {}/{}: dropping unsupported env entry {}: {}",
                        namespace, name, env.name, reason
                    );
                }
            }
        }

        // Build resource requirements
        let resources = Self::build_resource_requirements(&cluster.spec.resources);

        // Build volume claim template
        let volume_claim_templates = Self::build_volume_claim_templates(&cluster.spec.storage);

        // Config, TLS keypair, and (optionally) client CA volumes.
        let (volumes, mut volume_mounts) = Self::build_config_and_tls_volumes(cluster);
        volume_mounts.insert(
            0,
            VolumeMount {
                name: "data".to_string(),
                mount_path: "/data".to_string(),
                ..Default::default()
            },
        );

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
            volume_mounts: Some(volume_mounts),
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
                    // The broker exposes readiness under /health/ready; /ready
                    // is not served and made every pod fail its readiness gate.
                    path: Some("/health/ready".to_string()),
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
                // The published broker image runs as a non-root user. Secret
                // volume ownership is adjusted through fsGroup so TLS files
                // rendered 0440 are readable without making private keys
                // world-readable.
                security_context: Some(PodSecurityContext {
                    fs_group: Some(1000),
                    run_as_non_root: Some(true),
                    ..Default::default()
                }),
                node_selector: if cluster.spec.node_selector.is_empty() {
                    None
                } else {
                    Some(cluster.spec.node_selector.clone())
                },
                volumes: Some(volumes),
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
                service_name: format!("{name}-headless"),
                template: pod_template,
                volume_claim_templates: Some(volume_claim_templates),
                pod_management_policy: Some("Parallel".to_string()),
                update_strategy,
                ..Default::default()
            }),
            ..Default::default()
        };

        statefulset
    }

    /// Map a validated `spec.env[].valueFrom` reference onto the container's
    /// `EnvVarSource`.
    ///
    /// The reference is handed to the kubelet verbatim: the operator never
    /// reads the Secret or ConfigMap itself, so this needs no `get` on either
    /// resource and the shipped Role stays free of `secrets` access.
    fn build_env_var_source(source: EnvVarRef<'_>) -> k8s_openapi::api::core::v1::EnvVarSource {
        match source {
            EnvVarRef::Secret(secret) => k8s_openapi::api::core::v1::EnvVarSource {
                secret_key_ref: Some(k8s_openapi::api::core::v1::SecretKeySelector {
                    name: secret.name.clone(),
                    key: secret.key.clone(),
                    ..Default::default()
                }),
                ..Default::default()
            },
            EnvVarRef::ConfigMap(config_map) => k8s_openapi::api::core::v1::EnvVarSource {
                config_map_key_ref: Some(k8s_openapi::api::core::v1::ConfigMapKeySelector {
                    name: config_map.name.clone(),
                    key: config_map.key.clone(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        }
    }

    /// Build the config (and, when TLS is enabled, certificate) volumes plus
    /// their container mounts.
    ///
    /// TLS Secrets are mounted read-only so the broker can actually read the
    /// certificates referenced by the rendered configuration; previously the
    /// configuration pointed at paths that were never mounted.
    ///
    /// The TLS and CA mounts are siblings of the config mount, never nested
    /// below it: a Secret volume mounted inside the read-only ConfigMap mount
    /// is not a layout kubelet guarantees, and losing it silently would break
    /// TLS startup with a config file that still claims TLS is enabled.
    fn build_config_and_tls_volumes(
        cluster: &StreamlineCluster,
    ) -> (Vec<k8s_openapi::api::core::v1::Volume>, Vec<VolumeMount>) {
        use k8s_openapi::api::core::v1::{ConfigMapVolumeSource, SecretVolumeSource, Volume};

        let mut volumes = vec![Volume {
            name: "config".to_string(),
            config_map: Some(ConfigMapVolumeSource {
                name: format!("{}-config", cluster.name_any()),
                ..Default::default()
            }),
            ..Default::default()
        }];
        let mut mounts = vec![VolumeMount {
            name: "config".to_string(),
            mount_path: CONFIG_MOUNT_PATH.to_string(),
            read_only: Some(true),
            ..Default::default()
        }];

        if let Some(tls) = &cluster.spec.tls {
            if tls.enabled && !tls.secret_name.trim().is_empty() {
                volumes.push(Volume {
                    name: TLS_VOLUME_NAME.to_string(),
                    secret: Some(SecretVolumeSource {
                        secret_name: Some(tls.secret_name.clone()),
                        default_mode: Some(0o440),
                        ..Default::default()
                    }),
                    ..Default::default()
                });
                mounts.push(VolumeMount {
                    name: TLS_VOLUME_NAME.to_string(),
                    mount_path: TLS_MOUNT_PATH.to_string(),
                    read_only: Some(true),
                    ..Default::default()
                });

                if let Some(ca_secret) = tls.ca_secret_name.as_ref().filter(|_| tls.mtls_enabled) {
                    volumes.push(Volume {
                        name: TLS_CA_VOLUME_NAME.to_string(),
                        secret: Some(SecretVolumeSource {
                            secret_name: Some(ca_secret.clone()),
                            default_mode: Some(0o440),
                            ..Default::default()
                        }),
                        ..Default::default()
                    });
                    mounts.push(VolumeMount {
                        name: TLS_CA_VOLUME_NAME.to_string(),
                        mount_path: TLS_CA_MOUNT_PATH.to_string(),
                        read_only: Some(true),
                        ..Default::default()
                    });
                }
            }
        }

        (volumes, mounts)
    }

    /// Decide what the operator should do about this cluster's HPA.
    ///
    /// Always returns a configuration, so every reconcile makes a decision:
    /// `enabled: true` creates/updates the HPA, `enabled: false` deletes any
    /// HPA the operator owns.
    ///
    /// Returning `None` for "the spec never mentioned autoscaling" was the
    /// leak: deleting `spec.autoscaling` from a StreamlineCluster that had
    /// previously enabled it left the operator with nothing to reconcile, so
    /// the HPA it had created outlived the field that asked for it and kept
    /// scaling the StatefulSet. An absent block now means the same thing as a
    /// disabled one — the operator owns no HPA for this cluster — which is
    /// what makes the cleanup converge.
    ///
    /// The `enabled` flag is deliberately **not** copied straight from the
    /// spec. Autoscaling is rejected by [`ClusterSpec::validate`] until the
    /// operator can bootstrap raft peers, and a rejection that only refuses to
    /// create an HPA is incomplete: an HPA created by an earlier version of the
    /// operator (or by the same spec before this validation existed) survives
    /// the upgrade and keeps scaling the StatefulSet into multiple independent
    /// standalone brokers — exactly the split-brain the rejection exists to
    /// prevent. A rejected spec therefore yields a *disabled* configuration,
    /// which reconciles into a delete rather than a create.
    ///
    /// Pure function of the spec plus the validation verdict so the cleanup
    /// decision can be asserted without a Kubernetes API server.
    fn desired_hpa_config(
        spec: &crate::crd::ClusterSpec,
        spec_is_valid: bool,
    ) -> crate::controllers::autoscaling::AutoScalingConfig {
        let Some(autoscaling) = spec.autoscaling.as_ref() else {
            // No autoscaling block: the operator owns no HPA, so reconciling
            // means removing any HPA it left behind.
            return crate::controllers::autoscaling::AutoScalingConfig {
                enabled: false,
                ..Default::default()
            };
        };

        crate::controllers::autoscaling::AutoScalingConfig {
            // Only an autoscaling spec that is both requested *and* accepted
            // may result in an HPA; everything else is a cleanup.
            enabled: autoscaling.enabled && spec_is_valid,
            min_replicas: autoscaling.min_replicas,
            max_replicas: autoscaling.max_replicas,
            target_cpu_utilization: autoscaling.target_cpu_utilization,
            target_memory_utilization: autoscaling.target_memory_utilization,
            partition_aware: autoscaling.partition_aware,
            target_lag_per_partition: autoscaling.target_lag_per_partition,
            target_messages_per_second: autoscaling.target_messages_per_second,
            ..Default::default()
        }
    }

    /// Apply an autoscaling decision: create/update the HPA when enabled,
    /// delete any existing HPA when not.
    async fn reconcile_autoscaling(
        &self,
        cluster: &StreamlineCluster,
        namespace: &str,
        config: &crate::controllers::autoscaling::AutoScalingConfig,
    ) -> Result<()> {
        crate::controllers::autoscaling::AutoScalingController::new(self.client.clone())
            .reconcile_hpa(cluster, namespace, config)
            .await
    }

    /// Publish a Failed status for a spec the operator refuses to render.
    async fn update_status_invalid(
        &self,
        cluster: &StreamlineCluster,
        namespace: &str,
        message: &str,
    ) -> Result<()> {
        let status = Self::invalid_status(cluster, message);
        self.patch_status_if_changed(cluster, namespace, status)
            .await
    }

    /// Patch the cluster status, but only when it differs from what is already
    /// published.
    ///
    /// A status patch produces a watch event for the same object. Patching an
    /// unchanged status therefore re-enters `reconcile` immediately, which
    /// patches again — an unbounded hot loop that also rewrites
    /// `lastUpdated` and every condition timestamp on each pass. The status
    /// builders keep the existing timestamps for unchanged data so this
    /// comparison can actually succeed.
    async fn patch_status_if_changed(
        &self,
        cluster: &StreamlineCluster,
        namespace: &str,
        status: ClusterStatus,
    ) -> Result<()> {
        if cluster.status.as_ref() == Some(&status) {
            return Ok(());
        }

        let clusters: Api<StreamlineCluster> = Api::namespaced(self.client.clone(), namespace);
        clusters
            .patch_status(
                &cluster.name_any(),
                &PatchParams::default(),
                &Patch::Merge(&serde_json::json!({ "status": status })),
            )
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        Ok(())
    }

    /// Build the status published for a spec that failed validation.
    ///
    /// Pure function of the current object so the fail-closed path can be
    /// asserted, including its stability across repeated reconciles, without a
    /// Kubernetes API server.
    fn invalid_status(cluster: &StreamlineCluster, message: &str) -> ClusterStatus {
        let conditions = Self::invalid_spec_conditions(cluster, message);

        Self::with_stable_timestamp(
            cluster,
            ClusterStatus {
                phase: ClusterPhase::Failed,
                ready_replicas: 0,
                replicas: cluster.spec.replicas,
                leader_id: None,
                broker_endpoints: vec![],
                conditions,
                observed_generation: cluster.metadata.generation,
                last_updated: None,
            },
        )
    }

    /// Stamp `lastUpdated` only when the rest of the status actually changed.
    ///
    /// `lastUpdated` is excluded from the comparison: it is derived from the
    /// clock rather than from cluster state, so including it would make every
    /// status differ from the previous one and defeat the skip in
    /// [`Self::patch_status_if_changed`].
    fn with_stable_timestamp(
        cluster: &StreamlineCluster,
        mut status: ClusterStatus,
    ) -> ClusterStatus {
        let semantic_change = cluster.status.as_ref().is_none_or(|current| {
            let mut comparable = current.clone();
            comparable.last_updated = None;
            comparable != status
        });

        status.last_updated = if semantic_change {
            Some(Utc::now().to_rfc3339())
        } else {
            cluster
                .status
                .as_ref()
                .and_then(|current| current.last_updated.clone())
        };

        status
    }

    /// Seed the condition helper from the conditions already published.
    ///
    /// `set_condition` only preserves `lastTransitionTime` when it can see the
    /// previous condition; starting from an empty list would mint a fresh
    /// timestamp on every reconcile and make an otherwise identical status look
    /// changed.
    fn seeded_conditions(cluster: &StreamlineCluster) -> Vec<crate::conditions::ConditionFields> {
        cluster
            .status
            .as_ref()
            .map(|status| {
                status
                    .conditions
                    .iter()
                    .map(|condition| crate::conditions::ConditionFields {
                        condition_type: condition.r#type.clone(),
                        status: condition.status.clone(),
                        last_transition_time: condition.last_transition_time.clone(),
                        reason: condition.reason.clone(),
                        message: condition.message.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Conditions published when a spec fails validation.
    fn invalid_spec_conditions(
        cluster: &StreamlineCluster,
        message: &str,
    ) -> Vec<crate::crd::ClusterCondition> {
        let mut fields = Self::seeded_conditions(cluster);
        set_condition(
            &mut fields,
            build_condition(
                CLUSTER_CONDITION_READY,
                CONDITION_FALSE,
                "InvalidSpec",
                message,
            ),
        );
        set_condition(
            &mut fields,
            build_condition(
                CLUSTER_CONDITION_AVAILABLE,
                CONDITION_FALSE,
                "InvalidSpec",
                message,
            ),
        );
        set_condition(
            &mut fields,
            build_condition(
                CLUSTER_CONDITION_PROGRESSING,
                CONDITION_FALSE,
                "InvalidSpec",
                "Reconciliation is blocked until the spec is corrected",
            ),
        );
        set_condition(
            &mut fields,
            build_condition(
                CLUSTER_CONDITION_DEGRADED,
                CONDITION_TRUE,
                "InvalidSpec",
                message,
            ),
        );
        fields
            .into_iter()
            .map(|c| c.into_cluster_condition())
            .collect()
    }

    /// Update the cluster status with Kubernetes-standard conditions
    async fn update_status(&self, cluster: &StreamlineCluster, namespace: &str) -> Result<()> {
        let name = cluster.name_any();
        let pods: Api<Pod> = Api::namespaced(self.client.clone(), namespace);

        // Count ready pods
        let pod_list = pods
            .list(&ListParams::default().labels(&format!("app.kubernetes.io/instance={name}")))
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

        let status = Self::observed_status(cluster, namespace, ready_count);
        self.patch_status_if_changed(cluster, namespace, status)
            .await
    }

    /// Build the status for an observed broker count.
    ///
    /// Pure function of the object plus the ready-pod count so the healthy and
    /// degraded paths — and their stability across repeated reconciles — can be
    /// asserted without a Kubernetes API server.
    fn observed_status(
        cluster: &StreamlineCluster,
        namespace: &str,
        ready_count: i32,
    ) -> ClusterStatus {
        let name = cluster.name_any();
        let desired = cluster.spec.replicas;
        let all_ready = ready_count == desired;

        let phase = if all_ready {
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

        // Build Kubernetes-standard conditions, seeded from the conditions
        // already published so unchanged ones keep their transition timestamps.
        let mut cond_fields = Self::seeded_conditions(cluster);

        // Ready condition
        let (ready_status, ready_reason, ready_msg) = if all_ready {
            (
                CONDITION_TRUE,
                "AllBrokersReady",
                format!("{ready_count}/{desired} brokers ready"),
            )
        } else {
            (
                CONDITION_FALSE,
                "BrokersNotReady",
                format!("{ready_count}/{desired} brokers ready"),
            )
        };
        set_condition(
            &mut cond_fields,
            build_condition(
                CLUSTER_CONDITION_READY,
                ready_status,
                ready_reason,
                &ready_msg,
            ),
        );

        // Available condition — at least one broker is ready
        let (avail_status, avail_reason, avail_msg) = if ready_count > 0 {
            (
                CONDITION_TRUE,
                "MinimumAvailable",
                format!("{ready_count} broker(s) available"),
            )
        } else {
            (
                CONDITION_FALSE,
                "NoBrokersAvailable",
                "No brokers are available".to_string(),
            )
        };
        set_condition(
            &mut cond_fields,
            build_condition(
                CLUSTER_CONDITION_AVAILABLE,
                avail_status,
                avail_reason,
                &avail_msg,
            ),
        );

        // Progressing condition — rolling out or scaling
        let (prog_status, prog_reason, prog_msg) = if ready_count < desired {
            (
                CONDITION_TRUE,
                "ScalingUp",
                format!("Scaling from {ready_count} to {desired} replicas"),
            )
        } else if ready_count > desired {
            (
                CONDITION_TRUE,
                "ScalingDown",
                format!("Scaling down from {ready_count} to {desired} replicas"),
            )
        } else {
            (
                CONDITION_FALSE,
                "UpToDate",
                "All replicas are up to date".to_string(),
            )
        };
        set_condition(
            &mut cond_fields,
            build_condition(
                CLUSTER_CONDITION_PROGRESSING,
                prog_status,
                prog_reason,
                &prog_msg,
            ),
        );

        // Degraded is false only at the exact desired count. Zero, partial,
        // and excess ready pods all mean the observed state is not healthy.
        let (deg_status, deg_reason, deg_msg) = if all_ready {
            (CONDITION_FALSE, "Healthy", "Cluster is healthy".to_string())
        } else if ready_count == 0 {
            (
                CONDITION_TRUE,
                "NoBrokersReady",
                format!("No brokers are ready (0/{desired})"),
            )
        } else if ready_count < desired {
            (
                CONDITION_TRUE,
                "PartiallyReady",
                format!("Only {ready_count}/{desired} brokers ready"),
            )
        } else {
            (
                CONDITION_TRUE,
                "TooManyReadyBrokers",
                format!("{ready_count}/{desired} brokers ready; expected exactly {desired}"),
            )
        };
        set_condition(
            &mut cond_fields,
            build_condition(CLUSTER_CONDITION_DEGRADED, deg_status, deg_reason, &deg_msg),
        );

        let conditions = cond_fields
            .into_iter()
            .map(|c| c.into_cluster_condition())
            .collect();

        Self::with_stable_timestamp(
            cluster,
            ClusterStatus {
                phase,
                ready_replicas: ready_count,
                replicas: desired,
                leader_id: None,
                broker_endpoints,
                conditions,
                observed_generation: cluster.metadata.generation,
                last_updated: None,
            },
        )
    }

    fn common_labels(cluster: &StreamlineCluster) -> BTreeMap<String, String> {
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

    fn pod_selector(cluster: &StreamlineCluster) -> BTreeMap<String, String> {
        let mut labels = BTreeMap::new();
        labels.insert(
            "app.kubernetes.io/name".to_string(),
            "streamline".to_string(),
        );
        labels.insert("app.kubernetes.io/instance".to_string(), cluster.name_any());
        labels
    }

    fn owner_reference(cluster: &StreamlineCluster) -> OwnerReference {
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

    fn build_volume_claim_templates(storage: &ClusterStorage) -> Vec<PersistentVolumeClaim> {
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
    // unwrap/expect are acceptable in tests; the crate-wide lint targets production code.
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::crd::ClusterSpec;

    /// Build a cluster object from a JSON spec fragment; defaults fill the rest.
    fn cluster(spec_json: &str) -> StreamlineCluster {
        let spec: ClusterSpec = serde_json::from_str(spec_json).unwrap();
        let mut cluster = StreamlineCluster::new("test-cluster", spec);
        cluster.metadata.namespace = Some("streamline".to_string());
        cluster.metadata.uid = Some("uid-1234".to_string());
        cluster
    }

    fn status_condition<'a>(
        status: &'a ClusterStatus,
        condition_type: &str,
    ) -> &'a crate::crd::ClusterCondition {
        status
            .conditions
            .iter()
            .find(|condition| condition.r#type == condition_type)
            .unwrap_or_else(|| panic!("{condition_type} condition"))
    }

    /// The rendered broker pod template.
    fn statefulset_pod_spec(cluster: &StreamlineCluster) -> PodSpec {
        ClusterController::build_statefulset(cluster, "streamline")
            .spec
            .expect("statefulset spec")
            .template
            .spec
            .expect("pod spec")
    }

    /// The environment the broker container is rendered with.
    fn statefulset_env(cluster: &StreamlineCluster) -> Vec<EnvVar> {
        statefulset_pod_spec(cluster).containers[0]
            .env
            .clone()
            .expect("container env")
    }

    /// The single rendered variable called `name`.
    fn find_env<'a>(env: &'a [EnvVar], name: &str) -> &'a EnvVar {
        let matches: Vec<&EnvVar> = env.iter().filter(|e| e.name == name).collect();
        assert_eq!(
            matches.len(),
            1,
            "expected exactly one {name} variable, got {matches:?}"
        );
        matches[0]
    }

    #[test]
    fn config_map_renders_toml_broker_configuration() {
        let cluster = cluster("{}");
        let cm = ClusterController::build_configmap(&cluster, "streamline");

        let data = cm.data.expect("config map must carry data");
        let config = data
            .get(CONFIG_FILE_NAME)
            .expect("config map must contain the rendered config file");

        assert_eq!(CONFIG_FILE_NAME, "streamline.toml");
        assert!(config.contains("[server]"));
        assert!(config.contains("listen_addr = \"0.0.0.0:9092\""));
        assert!(config.contains("http_addr = \"0.0.0.0:9094\""));
        assert!(config.contains("data_dir = \"/data\""));
        assert!(config.contains("log_level = \"info\""));
        assert!(!config.contains(": "), "config must be TOML, not YAML");
        assert_eq!(cm.metadata.name.as_deref(), Some("test-cluster-config"));
    }

    /// The server's `ConfigFile` only defines `[server]`, `[storage]`, `[wal]`,
    /// `[tls]`, ... sections; keys it does not define are dead weight in the
    /// ConfigMap and mislead operators into thinking they take effect.
    #[test]
    fn rendered_config_omits_keys_the_server_does_not_define() {
        let cluster = cluster(r#"{"raftPort": 9095, "metricsEnabled": true}"#);
        let config = render_config(&cluster);

        assert!(
            !config.contains("raft_addr"),
            "the server config file has no raft_addr key: {config}"
        );
        assert!(
            !config.contains("metrics_enabled"),
            "the server config file has no metrics_enabled key: {config}"
        );
        assert!(
            !config.contains("9095"),
            "the raft port belongs on the Service, not in the config file: {config}"
        );
    }

    /// Every rendered key must live under the section the server parses it
    /// from: `[server]` for the listen/data/log settings, `[tls]` for TLS.
    #[test]
    fn rendered_keys_live_under_the_sections_the_server_parses() {
        let cluster = cluster(
            r#"{"tls": {"enabled": true, "secretName": "certs", "mtlsEnabled": true, "caSecretName": "ca"}}"#,
        );
        let config = render_config(&cluster);

        let mut section = String::new();
        let mut keys_by_section: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for line in config
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
        {
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                section = name.to_string();
                continue;
            }
            let key = line
                .split_once(" = ")
                .map(|(k, _)| k.to_string())
                .unwrap_or_else(|| panic!("line is not a TOML key/value: {line}"));
            assert!(
                !section.is_empty(),
                "key `{key}` rendered outside a section"
            );
            keys_by_section
                .entry(section.clone())
                .or_default()
                .push(key);
        }

        let section_keys =
            |name: &str| -> Vec<String> { keys_by_section.get(name).cloned().unwrap_or_default() };

        assert_eq!(
            section_keys("server"),
            vec!["listen_addr", "http_addr", "data_dir", "log_level"]
        );
        assert_eq!(
            section_keys("tls"),
            vec!["enabled", "cert", "key", "require_client_cert", "ca_cert"]
        );
    }

    #[test]
    fn rendered_config_is_wellformed_toml() {
        let cluster = cluster(
            r#"{"tls": {"enabled": true, "secretName": "certs", "mtlsEnabled": true, "caSecretName": "ca"}}"#,
        );
        let config = render_config(&cluster);

        for line in config
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
        {
            let is_table_header = line.starts_with('[') && line.ends_with(']');
            let is_key_value = line
                .split_once(" = ")
                .is_some_and(|(k, v)| !k.trim().is_empty() && !v.trim().is_empty());
            assert!(
                is_table_header || is_key_value,
                "line is not valid TOML: {line}"
            );
        }
    }

    #[test]
    fn tls_configuration_points_at_mounted_paths() {
        let cluster = cluster(
            r#"{"tls": {"enabled": true, "secretName": "certs", "mtlsEnabled": true, "caSecretName": "ca"}}"#,
        );
        let config = render_config(&cluster);

        assert!(config.contains("[tls]"));
        assert!(config.contains(&format!("cert = \"{TLS_MOUNT_PATH}/tls.crt\"")));
        assert!(config.contains(&format!("key = \"{TLS_MOUNT_PATH}/tls.key\"")));
        assert!(config.contains("require_client_cert = true"));
        assert!(config.contains(&format!("ca_cert = \"{TLS_CA_MOUNT_PATH}/ca.crt\"")));

        let sts = ClusterController::build_statefulset(&cluster, "streamline");
        let pod_spec = sts.spec.expect("spec").template.spec.expect("pod spec");
        let pod_security = pod_spec
            .security_context
            .as_ref()
            .expect("pod security context");
        assert_eq!(pod_security.fs_group, Some(1000));
        assert_eq!(pod_security.run_as_non_root, Some(true));

        let volumes = pod_spec.volumes.as_ref().expect("volumes");

        let tls_volume = volumes
            .iter()
            .find(|v| v.name == TLS_VOLUME_NAME)
            .expect("TLS secret must be mounted when TLS is enabled");
        assert_eq!(
            tls_volume
                .secret
                .as_ref()
                .and_then(|s| s.secret_name.as_deref()),
            Some("certs")
        );
        assert_eq!(
            tls_volume.secret.as_ref().and_then(|s| s.default_mode),
            Some(0o440)
        );

        let ca_volume = volumes
            .iter()
            .find(|v| v.name == TLS_CA_VOLUME_NAME)
            .expect("client CA secret must be mounted for mTLS");
        assert_eq!(
            ca_volume.secret.as_ref().and_then(|s| s.default_mode),
            Some(0o440)
        );

        let mounts = pod_spec.containers[0]
            .volume_mounts
            .as_ref()
            .expect("mounts");
        assert!(mounts
            .iter()
            .any(|m| m.mount_path == TLS_MOUNT_PATH && m.read_only == Some(true)));
        assert!(mounts
            .iter()
            .any(|m| m.mount_path == TLS_CA_MOUNT_PATH && m.read_only == Some(true)));
    }

    /// A Secret volume mounted *inside* the read-only ConfigMap mount is not a
    /// layout kubelet guarantees. If the TLS mount is shadowed or dropped, the
    /// broker starts with a config file that claims TLS is enabled while
    /// `cert`/`key` point at paths that do not exist.
    #[test]
    fn tls_mounts_are_siblings_of_the_config_mount_not_nested_inside_it() {
        let cluster = cluster(
            r#"{"tls": {"enabled": true, "secretName": "certs", "mtlsEnabled": true, "caSecretName": "ca"}}"#,
        );

        let config_prefix = format!("{CONFIG_MOUNT_PATH}/");
        for path in [TLS_MOUNT_PATH, TLS_CA_MOUNT_PATH] {
            assert!(
                !path.starts_with(&config_prefix),
                "{path} must not be nested below the read-only config mount {CONFIG_MOUNT_PATH}"
            );
        }
        assert_ne!(TLS_MOUNT_PATH, TLS_CA_MOUNT_PATH);
        assert_ne!(TLS_MOUNT_PATH, CONFIG_MOUNT_PATH);
        assert_ne!(TLS_CA_MOUNT_PATH, CONFIG_MOUNT_PATH);

        let sts = ClusterController::build_statefulset(&cluster, "streamline");
        let pod_spec = sts.spec.expect("spec").template.spec.expect("pod spec");
        let mounts = pod_spec.containers[0]
            .volume_mounts
            .clone()
            .expect("mounts");
        let paths: Vec<String> = mounts.iter().map(|m| m.mount_path.clone()).collect();

        // No mount path may be a strict prefix of another: nesting one volume
        // inside another is exactly the bug this guards against.
        for outer in &paths {
            for inner in &paths {
                if outer == inner {
                    continue;
                }
                assert!(
                    !inner.starts_with(&format!("{outer}/")),
                    "mount {inner} is nested inside mount {outer}: {paths:?}"
                );
            }
        }

        // The rendered configuration must point at the sibling paths.
        let config = render_config(&cluster);
        assert!(config.contains(&format!("cert = \"{TLS_MOUNT_PATH}/tls.crt\"")));
        assert!(config.contains(&format!("ca_cert = \"{TLS_CA_MOUNT_PATH}/ca.crt\"")));
        assert!(
            !config.contains("\"/etc/streamline/tls"),
            "config still references the nested TLS path: {config}"
        );
    }

    #[test]
    fn tls_volumes_absent_when_tls_disabled() {
        let cluster = cluster("{}");
        let sts = ClusterController::build_statefulset(&cluster, "streamline");
        let pod_spec = sts.spec.expect("spec").template.spec.expect("pod spec");
        let volumes = pod_spec.volumes.expect("volumes");

        assert_eq!(volumes.len(), 1);
        assert_eq!(volumes[0].name, "config");
        assert!(!render_config(&cluster).contains("[tls]"));
    }

    /// `spec.env[]` is the operator's only channel for broker settings the CRD
    /// does not model, and `valueFrom` is the only way to feed a broker a
    /// secret without putting it in the resource. It was accepted by the
    /// schema and then dropped during rendering, so a variable declared as a
    /// `secretKeyRef` reached the container as the empty string — a silently
    /// mis-configured (or unauthenticated) broker rather than a failure.
    #[test]
    fn spec_env_literal_values_are_rendered() {
        let cluster = cluster(r#"{"env": [{"name": "STREAMLINE_EXTRA", "value": "on"}]}"#);
        let env = statefulset_env(&cluster);
        let rendered = find_env(&env, "STREAMLINE_EXTRA");

        assert_eq!(rendered.value.as_deref(), Some("on"));
        assert!(
            rendered.value_from.is_none(),
            "a literal value must not render a reference"
        );
    }

    #[test]
    fn spec_env_secret_key_ref_is_rendered_as_a_reference() {
        let cluster = cluster(
            r#"{"env": [{"name": "SASL_PASSWORD",
                         "valueFrom": {"secretKeyRef": {"name": "broker-auth", "key": "password"}}}]}"#,
        );
        let env = statefulset_env(&cluster);
        let rendered = find_env(&env, "SASL_PASSWORD");

        assert!(
            rendered.value.is_none(),
            "a referenced variable must not also carry a literal value"
        );
        let secret = rendered
            .value_from
            .as_ref()
            .and_then(|source| source.secret_key_ref.as_ref())
            .expect("secretKeyRef must be rendered onto the container");
        assert_eq!(secret.name, "broker-auth");
        assert_eq!(secret.key, "password");

        // The kubelet resolves the reference at pod start; the operator must
        // not need (and does not have) read access to Secrets.
        assert_eq!(secret.optional, None);
    }

    #[test]
    fn spec_env_config_map_key_ref_is_rendered_as_a_reference() {
        let cluster = cluster(
            r#"{"env": [{"name": "STREAMLINE_TUNING",
                         "valueFrom": {"configMapKeyRef": {"name": "broker-tuning", "key": "flags"}}}]}"#,
        );
        let env = statefulset_env(&cluster);
        let rendered = find_env(&env, "STREAMLINE_TUNING");

        assert!(rendered.value.is_none());
        let config_map = rendered
            .value_from
            .as_ref()
            .and_then(|source| source.config_map_key_ref.as_ref())
            .expect("configMapKeyRef must be rendered onto the container");
        assert_eq!(config_map.name, "broker-tuning");
        assert_eq!(config_map.key, "flags");
        assert_eq!(config_map.optional, None);
    }

    /// Rendering must never turn a reference into an empty variable, even if a
    /// spec that `ClusterSpec::validate` rejects somehow reaches the renderer.
    #[test]
    fn unmappable_env_entries_are_never_rendered_as_empty_variables() {
        let unmappable = [
            r#"{"env": [{"name": "SASL_PASSWORD"}]}"#,
            r#"{"env": [{"name": "SASL_PASSWORD", "valueFrom": {}}]}"#,
            r#"{"env": [{"name": "SASL_PASSWORD", "value": "literal",
                         "valueFrom": {"secretKeyRef": {"name": "s", "key": "k"}}}]}"#,
            r#"{"env": [{"name": "SASL_PASSWORD",
                         "valueFrom": {"secretKeyRef": {"name": "s", "key": "k"},
                                       "configMapKeyRef": {"name": "c", "key": "k"}}}]}"#,
            r#"{"env": [{"name": "SASL_PASSWORD",
                         "valueFrom": {"secretKeyRef": {"name": "", "key": "password"}}}]}"#,
        ];

        for spec in unmappable {
            let cluster = cluster(spec);
            assert!(
                cluster.spec.validate().is_err(),
                "{spec} must be rejected before it reaches the renderer"
            );

            let env = statefulset_env(&cluster);
            assert!(
                !env.iter().any(|e| e.name == "SASL_PASSWORD"),
                "{spec} rendered SASL_PASSWORD instead of dropping it: {env:?}"
            );
        }
    }

    /// The operator's own downward-API variables must keep working alongside
    /// user-supplied references.
    #[test]
    fn operator_env_survives_alongside_spec_env() {
        let cluster = cluster(
            r#"{"env": [{"name": "SASL_PASSWORD",
                         "valueFrom": {"secretKeyRef": {"name": "broker-auth", "key": "password"}}}]}"#,
        );
        let env = statefulset_env(&cluster);

        assert_eq!(
            find_env(&env, "STREAMLINE_CONFIG").value.as_deref(),
            Some(CONFIG_FILE_PATH)
        );
        let pod_name = find_env(&env, "POD_NAME");
        assert_eq!(
            pod_name
                .value_from
                .as_ref()
                .and_then(|source| source.field_ref.as_ref())
                .map(|field| field.field_path.as_str()),
            Some("metadata.name")
        );
    }

    /// Every rendered variable must carry a value or a source; a variable with
    /// neither is the empty-string bug this guards against.
    #[test]
    fn no_rendered_env_var_is_left_without_a_value_or_a_source() {
        let cluster = cluster(
            r#"{"env": [{"name": "LITERAL", "value": "x"},
                        {"name": "DELIBERATELY_EMPTY", "value": ""},
                        {"name": "FROM_SECRET",
                         "valueFrom": {"secretKeyRef": {"name": "s", "key": "k"}}},
                        {"name": "FROM_CONFIG_MAP",
                         "valueFrom": {"configMapKeyRef": {"name": "c", "key": "k"}}}]}"#,
        );
        assert!(cluster.spec.validate().is_ok());

        for rendered in statefulset_env(&cluster) {
            assert!(
                rendered.value.is_some() || rendered.value_from.is_some(),
                "{} would reach the broker as an empty string",
                rendered.name
            );
        }
    }

    // --- Scheduling -------------------------------------------------------

    /// `nodeSelector` is the one scheduling setting the pod template actually
    /// carries, so it must keep being rendered.
    #[test]
    fn node_selector_is_rendered_onto_the_pod_template() {
        let cluster = cluster(r#"{"nodeSelector": {"disktype": "ssd"}}"#);
        let pod_spec = statefulset_pod_spec(&cluster);

        assert_eq!(
            pod_spec
                .node_selector
                .as_ref()
                .and_then(|selector| selector.get("disktype"))
                .map(String::as_str),
            Some("ssd")
        );
    }

    /// The pod template renders no affinity, topology spread, or tolerations.
    /// `ClusterSpec::validate` rejects those settings for exactly that reason;
    /// this pins the render side so the rejection cannot quietly become wrong.
    #[test]
    fn no_scheduling_settings_beyond_node_selector_are_rendered() {
        let cluster = cluster(r#"{"nodeSelector": {"disktype": "ssd"}}"#);
        let pod_spec = statefulset_pod_spec(&cluster);

        assert!(
            pod_spec.affinity.is_none(),
            "podAntiAffinity is rejected by validation because nothing renders affinity"
        );
        assert!(
            pod_spec.tolerations.is_none(),
            "tolerations are rejected by validation because nothing renders them"
        );
        assert!(
            pod_spec.topology_spread_constraints.is_none(),
            "rackAwareness is rejected by validation because nothing renders topology spread"
        );
    }

    #[test]
    fn statefulset_wires_config_probes_and_storage() {
        let cluster = cluster("{}");
        let sts = ClusterController::build_statefulset(&cluster, "streamline");

        let spec = sts.spec.expect("statefulset spec");
        let pod_spec = spec.template.spec.clone().expect("pod spec");
        let container = &pod_spec.containers[0];

        assert_eq!(container.name, "streamline");
        assert_eq!(spec.service_name, "test-cluster-headless");
        assert_eq!(spec.replicas, Some(1));

        let mounts = container.volume_mounts.as_ref().expect("volume mounts");
        assert!(mounts.iter().any(|m| m.mount_path == CONFIG_MOUNT_PATH));
        assert!(mounts.iter().any(|m| m.mount_path == "/data"));

        // The broker must be told where its rendered configuration lives.
        let env = container.env.as_ref().expect("env");
        let config_env = env
            .iter()
            .find(|e| e.name == "STREAMLINE_CONFIG")
            .expect("STREAMLINE_CONFIG must be set");
        assert_eq!(config_env.value.as_deref(), Some(CONFIG_FILE_PATH));
    }

    #[test]
    fn readiness_probe_uses_the_broker_ready_path() {
        let cluster = cluster("{}");
        let sts = ClusterController::build_statefulset(&cluster, "streamline");
        let container = sts
            .spec
            .expect("spec")
            .template
            .spec
            .expect("pod")
            .containers
            .remove(0);

        let readiness = container
            .readiness_probe
            .and_then(|p| p.http_get)
            .expect("readiness probe");
        assert_eq!(readiness.path.as_deref(), Some("/health/ready"));

        let liveness = container
            .liveness_probe
            .and_then(|p| p.http_get)
            .expect("liveness probe");
        assert_eq!(liveness.path.as_deref(), Some("/health"));
    }

    #[test]
    fn broker_image_defaults_to_the_server_image() {
        let cluster = cluster("{}");
        let sts = ClusterController::build_statefulset(&cluster, "streamline");
        let container = sts
            .spec
            .expect("spec")
            .template
            .spec
            .expect("pod")
            .containers
            .remove(0);

        let image = container.image.expect("image");
        assert!(
            !image.contains("streamline-operator"),
            "brokers must not run the operator image, got {image}"
        );
    }

    #[test]
    fn invalid_spec_conditions_report_the_reason() {
        let cluster = cluster("{}");
        let conditions =
            ClusterController::invalid_spec_conditions(&cluster, "tls.secretName is required");

        let ready = conditions
            .iter()
            .find(|c| c.r#type == CLUSTER_CONDITION_READY)
            .expect("Ready condition");
        assert_eq!(ready.status, CONDITION_FALSE);
        assert_eq!(ready.reason.as_deref(), Some("InvalidSpec"));

        let degraded = conditions
            .iter()
            .find(|c| c.r#type == CLUSTER_CONDITION_DEGRADED)
            .expect("Degraded condition");
        assert_eq!(degraded.status, CONDITION_TRUE);
        assert!(degraded
            .message
            .as_deref()
            .is_some_and(|m| m.contains("tls.secretName")));
    }

    #[test]
    fn zero_ready_replicas_are_neither_ready_nor_healthy() {
        let resource = cluster(r#"{"replicas": 3}"#);
        let status = ClusterController::observed_status(&resource, "streamline", 0);

        assert_eq!(status.phase, ClusterPhase::Pending);
        assert_eq!(
            status_condition(&status, CLUSTER_CONDITION_READY).status,
            CONDITION_FALSE
        );
        let degraded = status_condition(&status, CLUSTER_CONDITION_DEGRADED);
        assert_eq!(degraded.status, CONDITION_TRUE);
        assert_eq!(degraded.reason.as_deref(), Some("NoBrokersReady"));
    }

    #[test]
    fn partially_ready_replicas_are_neither_ready_nor_healthy() {
        let resource = cluster(r#"{"replicas": 3}"#);
        let status = ClusterController::observed_status(&resource, "streamline", 2);

        assert_eq!(status.phase, ClusterPhase::Scaling);
        assert_eq!(
            status_condition(&status, CLUSTER_CONDITION_READY).status,
            CONDITION_FALSE
        );
        let degraded = status_condition(&status, CLUSTER_CONDITION_DEGRADED);
        assert_eq!(degraded.status, CONDITION_TRUE);
        assert_eq!(degraded.reason.as_deref(), Some("PartiallyReady"));
    }

    #[test]
    fn exactly_the_desired_replicas_are_ready_and_healthy() {
        let resource = cluster(r#"{"replicas": 3}"#);
        let status = ClusterController::observed_status(&resource, "streamline", 3);

        assert_eq!(status.phase, ClusterPhase::Running);
        assert_eq!(
            status_condition(&status, CLUSTER_CONDITION_READY).status,
            CONDITION_TRUE
        );
        let degraded = status_condition(&status, CLUSTER_CONDITION_DEGRADED);
        assert_eq!(degraded.status, CONDITION_FALSE);
        assert_eq!(degraded.reason.as_deref(), Some("Healthy"));
    }

    #[test]
    fn excess_ready_replicas_are_neither_ready_nor_healthy() {
        let resource = cluster("{}");
        let status = ClusterController::observed_status(&resource, "streamline", 2);

        assert_eq!(status.phase, ClusterPhase::Scaling);
        assert_eq!(
            status_condition(&status, CLUSTER_CONDITION_READY).status,
            CONDITION_FALSE
        );
        let progressing = status_condition(&status, CLUSTER_CONDITION_PROGRESSING);
        assert_eq!(progressing.status, CONDITION_TRUE);
        assert_eq!(progressing.reason.as_deref(), Some("ScalingDown"));
        let degraded = status_condition(&status, CLUSTER_CONDITION_DEGRADED);
        assert_eq!(degraded.status, CONDITION_TRUE);
        assert_eq!(degraded.reason.as_deref(), Some("TooManyReadyBrokers"));
    }

    // --- Status stability -------------------------------------------------
    //
    // A status patch generates a watch event for the same object. If the
    // controller rebuilds `lastUpdated` and every condition timestamp on each
    // pass, the patch is never a no-op and the controller re-triggers itself
    // forever. These tests pin the fail-closed and healthy paths to a status
    // that compares equal when nothing changed.

    #[test]
    fn repeated_invalid_status_is_stable() {
        let mut resource = cluster(r#"{"autoscaling": {"enabled": true}}"#);
        let message = "autoscaling.enabled is not supported";

        let first = ClusterController::invalid_status(&resource, message);
        assert_eq!(first.phase, ClusterPhase::Failed);
        resource.status = Some(first.clone());

        let second = ClusterController::invalid_status(&resource, message);
        assert_eq!(
            second, first,
            "an unchanged invalid spec must produce an identical status"
        );
    }

    #[test]
    fn repeated_healthy_status_is_stable() {
        let mut resource = cluster("{}");

        let first = ClusterController::observed_status(&resource, "streamline", 1);
        assert_eq!(first.phase, ClusterPhase::Running);
        assert_eq!(first.ready_replicas, 1);
        resource.status = Some(first.clone());

        let second = ClusterController::observed_status(&resource, "streamline", 1);
        assert_eq!(
            second, first,
            "an unchanged healthy cluster must produce an identical status"
        );
    }

    #[test]
    fn repeated_non_exact_statuses_are_stable() {
        for ready_count in [0, 2, 4] {
            let mut resource = cluster(r#"{"replicas": 3}"#);
            let first = ClusterController::observed_status(&resource, "streamline", ready_count);
            resource.status = Some(first.clone());

            let second = ClusterController::observed_status(&resource, "streamline", ready_count);
            assert_eq!(
                second, first,
                "ready_count={ready_count} must not churn status timestamps"
            );
        }
    }

    #[test]
    fn real_change_updates_status_without_resetting_unchanged_transition_times() {
        let mut resource = cluster(r#"{"replicas": 3}"#);
        let mut first = ClusterController::observed_status(&resource, "streamline", 1);
        first.last_updated = Some("2024-01-01T00:00:00Z".to_string());

        let degraded_transition = first
            .conditions
            .iter()
            .find(|c| c.r#type == CLUSTER_CONDITION_DEGRADED)
            .and_then(|c| c.last_transition_time.clone())
            .expect("Degraded transition time");
        resource.status = Some(first);

        // Another broker became ready, but the cluster is still partial:
        // condition statuses stay unchanged while the observed count changes.
        let second = ClusterController::observed_status(&resource, "streamline", 2);
        assert_eq!(second.phase, ClusterPhase::Scaling);
        assert_ne!(
            second.last_updated.as_deref(),
            Some("2024-01-01T00:00:00Z"),
            "a real state change must refresh lastUpdated"
        );
        assert_eq!(
            second
                .conditions
                .iter()
                .find(|c| c.r#type == CLUSTER_CONDITION_DEGRADED)
                .and_then(|c| c.last_transition_time.clone()),
            Some(degraded_transition),
            "a still-degraded condition must keep its transition time"
        );
    }

    #[test]
    fn status_only_differing_in_timestamp_is_not_treated_as_a_change() {
        let mut resource = cluster("{}");
        let mut published = ClusterController::observed_status(&resource, "streamline", 1);
        published.last_updated = Some("2024-01-01T00:00:00Z".to_string());
        resource.status = Some(published.clone());

        let next = ClusterController::observed_status(&resource, "streamline", 1);
        assert_eq!(
            next.last_updated.as_deref(),
            Some("2024-01-01T00:00:00Z"),
            "lastUpdated must be preserved when nothing else changed"
        );
        assert_eq!(next, published);
    }

    // --- Autoscaling rejection cleanup ------------------------------------
    //
    // Rejecting `autoscaling.enabled` only prevents the operator from creating
    // a *new* HPA. An HPA created by an earlier version survives the upgrade
    // and keeps scaling the StatefulSet into multiple independent standalone
    // brokers — the split-brain the rejection exists to prevent.

    #[test]
    fn rejected_autoscaling_deletes_the_stale_hpa_and_creates_none() {
        let spec: ClusterSpec =
            serde_json::from_str(r#"{"autoscaling": {"enabled": true}}"#).unwrap();
        assert!(
            spec.validate().is_err(),
            "enabled autoscaling must still be rejected"
        );

        let config = ClusterController::desired_hpa_config(&spec, false);

        assert!(
            !config.enabled,
            "a rejected autoscaling spec must not leave the HPA enabled"
        );
        assert_eq!(
            crate::controllers::autoscaling::hpa_action(&config),
            crate::controllers::autoscaling::HpaAction::Delete,
            "cleanup must delete the stale HPA rather than apply a new one"
        );
    }

    #[test]
    fn explicitly_disabled_autoscaling_also_requests_cleanup() {
        let spec: ClusterSpec =
            serde_json::from_str(r#"{"autoscaling": {"enabled": false}}"#).unwrap();
        assert!(spec.validate().is_ok());

        let config = ClusterController::desired_hpa_config(&spec, true);
        assert_eq!(
            crate::controllers::autoscaling::hpa_action(&config),
            crate::controllers::autoscaling::HpaAction::Delete
        );
    }

    /// Deleting the `autoscaling` block must not orphan the HPA it created.
    /// Every reconcile of a cluster without autoscaling therefore reconciles a
    /// *disabled* configuration, which deletes any operator-owned HPA.
    #[test]
    fn absent_autoscaling_still_reconciles_a_cleanup() {
        let spec: ClusterSpec = serde_json::from_str("{}").unwrap();
        let config = ClusterController::desired_hpa_config(&spec, true);

        assert!(!config.enabled);
        assert_eq!(
            crate::controllers::autoscaling::hpa_action(&config),
            crate::controllers::autoscaling::HpaAction::Delete,
            "an absent autoscaling block must still remove a stale HPA"
        );
    }

    /// An invalid spec that never mentioned autoscaling must also converge on
    /// cleanup: the reconcile returns early after the validation gate, so the
    /// cleanup has to happen before it.
    #[test]
    fn an_invalid_spec_without_autoscaling_still_reconciles_a_cleanup() {
        let spec: ClusterSpec = serde_json::from_str(r#"{"replicas": 3}"#).unwrap();
        assert!(spec.validate().is_err());

        let config = ClusterController::desired_hpa_config(&spec, false);
        assert_eq!(
            crate::controllers::autoscaling::hpa_action(&config),
            crate::controllers::autoscaling::HpaAction::Delete
        );
    }

    /// The cleanup must not become a back door that re-enables autoscaling: an
    /// HPA is only ever applied for a spec that both requests it and passes
    /// validation.
    #[test]
    fn an_hpa_is_only_applied_for_a_requested_and_accepted_spec() {
        let spec: ClusterSpec =
            serde_json::from_str(r#"{"autoscaling": {"enabled": true, "maxReplicas": 5}}"#)
                .unwrap();

        let rejected = ClusterController::desired_hpa_config(&spec, false);
        assert!(!rejected.enabled);

        let accepted = ClusterController::desired_hpa_config(&spec, true);
        assert!(accepted.enabled);
        assert_eq!(accepted.max_replicas, 5);
        assert_eq!(
            crate::controllers::autoscaling::hpa_action(&accepted),
            crate::controllers::autoscaling::HpaAction::Apply
        );
    }

    /// Cleaning up the HPA must not soften the single-replica rejection.
    #[test]
    fn single_replica_rejection_is_unchanged() {
        let spec: ClusterSpec = serde_json::from_str(r#"{"replicas": 3}"#).unwrap();
        let errors = spec.validate().unwrap_err();
        assert!(errors
            .iter()
            .any(|e| e.contains("replicas=3 is not supported")));

        let autoscaled: ClusterSpec =
            serde_json::from_str(r#"{"autoscaling": {"enabled": true}}"#).unwrap();
        assert!(autoscaled
            .validate()
            .unwrap_err()
            .iter()
            .any(|e| e.contains("autoscaling.enabled is not supported")));
    }
}
