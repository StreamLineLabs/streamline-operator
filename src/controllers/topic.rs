//! Topic Controller
//!
//! Reconciles StreamlineTopic custom resources to create and manage
//! topics within Streamline clusters.

use crate::conditions::{
    build_condition, set_condition, ConditionFields, CONDITION_FALSE, CONDITION_TRUE,
    CONDITION_UNKNOWN, TOPIC_CONDITION_READY, TOPIC_CONDITION_SYNCED, TOPIC_FINALIZER,
};
use crate::controllers::{error_policy_backoff, WatchScope};
use crate::crd::{StreamlineCluster, StreamlineTopic, TopicPhase, TopicStatus};
use crate::error::{OperatorError, Result};
use chrono::Utc;
use futures::StreamExt;
use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher::Config;
use kube::{Client, ResourceExt};
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

/// Context for the topic controller
pub struct TopicController {
    client: Client,
    http_client: reqwest::Client,
    scope: WatchScope,
}

/// State the Streamline server reports for a topic.
///
/// Fields are optional because the server is not required to echo the whole
/// topic back; the operator must not invent values it was not told.
///
/// Deliberately *only* the two fields the core `TopicDetails` response carries
/// that this operator can act on. `TopicDetails.config` is also returned by
/// `GET /api/v1/topics/{name}`, but it reports the **server's own** defaults
/// (`retention.ms: -1`, `segment.bytes: 104857600`, …) rather than anything the
/// operator asked for — the create endpoint has no way to accept topic config
/// at all. Parsing it into "the spec is in sync" would manufacture agreement
/// out of two unrelated sets of defaults.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TopicServerState {
    pub partitions: Option<i32>,
    pub replication_factor: Option<i32>,
}

impl TopicServerState {
    fn is_empty(&self) -> bool {
        self.partitions.is_none() && self.replication_factor.is_none()
    }

    /// Whether the server reported every field the operator verifies.
    fn is_fully_reported(&self) -> bool {
        self.partitions.is_some() && self.replication_factor.is_some()
    }
}

/// Outcome of comparing the desired spec with what the server reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SyncOutcome {
    /// The server reported values matching the spec.
    InSync(TopicServerState),
    /// The server reported values that differ from the spec.
    Drift(String),
    /// The server accepted the request but reported nothing to verify against.
    Unverified,
}

/// Parse the topic state out of a Streamline API response body.
///
/// Accepts both a bare topic object and one wrapped in `{"topic": {...}}`, and
/// tolerates the `partitionCount`/`partition_count` spellings.
pub(crate) fn parse_topic_state(body: &str) -> TopicServerState {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(body) else {
        return TopicServerState::default();
    };
    let root = json.get("topic").unwrap_or(&json);

    let number = |keys: &[&str]| -> Option<i32> {
        keys.iter()
            .find_map(|k| root.get(*k).and_then(serde_json::Value::as_i64))
            .and_then(|v| i32::try_from(v).ok())
    };

    TopicServerState {
        partitions: number(&["partitions", "partition_count", "partitionCount"]),
        replication_factor: number(&["replication_factor", "replicationFactor"]),
    }
}

/// Topic settings the Streamline server API cannot honour today.
///
/// The core topic API (`POST /api/v1/topics`) accepts exactly two fields:
///
/// ```json
/// { "name": "events", "partitions": 3 }
/// ```
///
/// Everything else in the request body is discarded by the server's request
/// type, replication is hard-coded to 1, and there is no update endpoint at
/// all. The operator used to POST `retention_ms`, `cleanup_policy`,
/// `compression_type`, `min_insync_replicas`, `max_message_bytes` and
/// `segment_bytes` anyway and then reported `Ready`/`Synced`, which promised
/// retention and compaction behaviour the broker never applied — silent data
/// loss for anyone relying on a compaction policy or a retention bound.
///
/// So every setting the server cannot apply is rejected *before* the API call
/// whenever it differs from the CRD default. Leaving a field at its schema
/// default is accepted: the user asked for nothing, and nothing is what the
/// server does. Setting it is refused rather than silently dropped.
pub(crate) fn unsupported_fields(spec: &crate::crd::TopicSpec) -> Vec<String> {
    let mut unsupported = Vec::new();

    if spec.replication_factor != 1 {
        // v0.3.0 defaulted this to 2, so topics created against those CRDs
        // carry a replication factor nobody asked for and the server never
        // honoured. Point at the upgrade path rather than only refusing.
        unsupported.push(format!(
            "replicationFactor={} is not supported: the Streamline topic API creates \
             single-replica topics (set replicationFactor: 1), so this topic would claim \
             durability the cluster does not provide.{}",
            spec.replication_factor,
            crate::upgrade::remediation_for(
                "spec.replicationFactor",
                spec.replication_factor == crate::upgrade::LEGACY_TOPIC_REPLICATION_FACTOR
            )
        ));
    }

    if spec.partitions < 1 {
        unsupported.push(format!("partitions must be >= 1, got {}", spec.partitions));
    }

    // --- Settings the create endpoint silently drops ----------------------
    let defaults = crate::crd::TopicSpec::config_defaults();

    // `field` is the leaf path as the status message names it
    // (`retention.retentionMs`); the upgrade table keys on the full spec path.
    // Three of these settings are v0.3.0 schema defaults, so the rejection
    // carries the patch that clears them — and, because provenance is decided
    // from the observed value rather than from the field, a deliberate
    // `retentionMs: 3600000` is still rejected with a fix but is never blamed
    // on an upgrade the user did not perform.
    let mut reject = |field: &str, requested: String, default: String| {
        let remediation =
            crate::upgrade::remediation_for_value(&format!("spec.{field}"), &requested);
        unsupported.push(format!(
            "{field}={requested} is not supported: the Streamline topic API applies no topic \
             configuration (it accepts only name and partitions), so this setting would be \
             silently discarded (leave it at the default {default}).{remediation}"
        ));
    };

    if spec.retention.retention_ms != defaults.retention_ms {
        reject(
            "retention.retentionMs",
            spec.retention.retention_ms.to_string(),
            defaults.retention_ms.to_string(),
        );
    }
    if spec.retention.retention_bytes != defaults.retention_bytes {
        reject(
            "retention.retentionBytes",
            spec.retention.retention_bytes.to_string(),
            defaults.retention_bytes.to_string(),
        );
    }
    if spec.retention.cleanup_policy != defaults.cleanup_policy {
        reject(
            "retention.cleanupPolicy",
            spec.retention.cleanup_policy.clone(),
            defaults.cleanup_policy.clone(),
        );
    }
    if spec.compression.r#type != defaults.compression_type {
        reject(
            "compression.type",
            spec.compression.r#type.clone(),
            defaults.compression_type.clone(),
        );
    }

    // Every `spec.config` entry is an override, so any value at all is a
    // request the server cannot satisfy.
    for (field, value) in [
        (
            "config.minInsyncReplicas",
            spec.config.min_insync_replicas.map(|v| v.to_string()),
        ),
        (
            "config.maxMessageBytes",
            spec.config.max_message_bytes.map(|v| v.to_string()),
        ),
        (
            "config.segmentBytes",
            spec.config.segment_bytes.map(|v| v.to_string()),
        ),
        (
            "config.indexIntervalBytes",
            spec.config.index_interval_bytes.map(|v| v.to_string()),
        ),
        (
            "config.flushIntervalMs",
            spec.config.flush_interval_ms.map(|v| v.to_string()),
        ),
        (
            "config.flushMessages",
            spec.config.flush_messages.map(|v| v.to_string()),
        ),
    ] {
        if let Some(value) = value {
            reject(field, value, "unset".to_string());
        }
    }

    for key in spec.config.custom.keys() {
        reject(
            &format!("config.custom[{key}]"),
            "set".to_string(),
            "unset".to_string(),
        );
    }

    unsupported
}

/// Compare the desired spec against what the server reported.
///
/// Only [`TopicServerState`] — partition count and replication factor — is
/// verifiable, and both must be present before the topic is called in sync.
/// A partial answer is [`SyncOutcome::Unverified`], which publishes
/// `Synced=Unknown` instead of claiming agreement the server never confirmed.
pub(crate) fn evaluate_sync(spec: &crate::crd::TopicSpec, state: TopicServerState) -> SyncOutcome {
    let mut drift = Vec::new();
    if let Some(partitions) = state.partitions {
        if partitions != spec.partitions {
            drift.push(format!(
                "server reports {} partitions, spec requests {}",
                partitions, spec.partitions
            ));
        }
    }
    if let Some(rf) = state.replication_factor {
        if rf != spec.replication_factor {
            drift.push(format!(
                "server reports replication factor {}, spec requests {}",
                rf, spec.replication_factor
            ));
        }
    }

    if !drift.is_empty() {
        return SyncOutcome::Drift(drift.join("; "));
    }

    if state.is_fully_reported() {
        SyncOutcome::InSync(state)
    } else {
        SyncOutcome::Unverified
    }
}

impl TopicController {
    /// Create a new topic controller watching `scope`.
    pub fn new(client: Client, http_client: reqwest::Client, scope: WatchScope) -> Self {
        Self {
            client,
            http_client,
            scope,
        }
    }

    /// The settings in `spec` the Streamline server cannot apply, worded
    /// exactly as the controller publishes them in `status`.
    ///
    /// Exposed so the fail-closed gate can be exercised against a stored
    /// resource without a cluster, an HTTP client, or a running controller —
    /// `tests/upgrade_from_v0_3_0.rs` deserializes v0.3.0-shaped topics and
    /// asserts on the reasons this returns.
    #[must_use]
    pub fn unsupported_fields(spec: &crate::crd::TopicSpec) -> Vec<String> {
        unsupported_fields(spec)
    }

    /// Run the topic controller
    pub async fn run(self: Arc<Self>) -> Result<()> {
        let topics: Api<StreamlineTopic> = self.scope.api(self.client.clone());

        info!(
            "Starting StreamlineTopic controller (watching {})",
            self.scope.describe()
        );

        Controller::new(topics, Config::default())
            .shutdown_on_signal()
            .run(
                |topic, ctx| async move { ctx.reconcile(topic).await },
                |_topic, error, _ctx| {
                    error!("Reconciliation error: {:?}", error);
                    crate::metrics::get().inc_error("topic");
                    error_policy_backoff(_topic, error, _ctx)
                },
                Arc::clone(&self),
            )
            .for_each(|result| async move {
                match result {
                    Ok((obj, _action)) => {
                        info!("Reconciled topic: {}", obj.name);
                    }
                    Err(e) => {
                        error!("Reconciliation failed: {:?}", e);
                    }
                }
            })
            .await;

        Ok(())
    }

    /// Reconcile a StreamlineTopic
    async fn reconcile(
        &self,
        topic: Arc<StreamlineTopic>,
    ) -> std::result::Result<Action, OperatorError> {
        crate::metrics::get().inc_reconcile("topic");
        let _timer = crate::metrics::get().start_timer();
        let name = topic.name_any();
        let namespace = topic.namespace().unwrap_or_else(|| "default".to_string());

        info!("Reconciling StreamlineTopic {}/{}", namespace, name);

        // Handle deletion with finalizer
        if topic.metadata.deletion_timestamp.is_some() {
            return self.handle_deletion(&topic, &namespace).await;
        }

        // Ensure finalizer is set
        self.ensure_finalizer(&topic, &namespace).await?;

        // Reject settings the server cannot honour before touching the cluster:
        // reporting Ready for an unsupported spec would misrepresent durability.
        let unsupported = unsupported_fields(&topic.spec);
        if !unsupported.is_empty() {
            let message = unsupported.join("; ");
            warn!(
                "StreamlineTopic {}/{} requests unsupported settings: {}",
                namespace, name, message
            );
            self.update_status_unsupported(&topic, &namespace, &message)
                .await?;
            crate::metrics::get().inc_error("topic");
            return Ok(Action::requeue(Duration::from_secs(300)));
        }

        // Get the referenced cluster
        let clusters: Api<StreamlineCluster> = Api::namespaced(self.client.clone(), &namespace);
        let cluster = match clusters.get(&topic.spec.cluster_ref).await {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    "Cluster {} not found for topic {}: {}",
                    topic.spec.cluster_ref, name, e
                );
                self.update_status_error(
                    &topic,
                    &namespace,
                    &format!("Cluster {} not found", topic.spec.cluster_ref),
                )
                .await?;
                return Ok(Action::requeue(Duration::from_secs(30)));
            }
        };

        // Check if cluster is ready
        let cluster_ready = cluster
            .status
            .as_ref()
            .is_some_and(|s| s.ready_replicas > 0 && !s.broker_endpoints.is_empty());

        if !cluster_ready {
            warn!(
                "Cluster {} not ready for topic {}",
                topic.spec.cluster_ref, name
            );
            self.update_status_pending(&topic, &namespace, "Waiting for cluster to be ready")
                .await?;
            return Ok(Action::requeue(Duration::from_secs(10)));
        }

        // Create/update topic via Streamline HTTP API
        match self.create_or_update_topic(&topic, &cluster).await {
            Ok(state) => {
                self.update_status_from_server(&topic, &namespace, state)
                    .await?;
            }
            Err(e) => {
                error!("Failed to create/update topic {}: {}", name, e);
                self.update_status_error(&topic, &namespace, &e.to_string())
                    .await?;
                return Ok(Action::requeue(Duration::from_secs(30)));
            }
        }

        crate::metrics::get().inc_success();
        Ok(Action::requeue(Duration::from_secs(60)))
    }

    /// Ensure the finalizer is present on the resource
    async fn ensure_finalizer(&self, topic: &StreamlineTopic, namespace: &str) -> Result<()> {
        let finalizers = topic.metadata.finalizers.as_deref().unwrap_or_default();
        if finalizers.contains(&TOPIC_FINALIZER.to_string()) {
            return Ok(());
        }

        let topics: Api<StreamlineTopic> = Api::namespaced(self.client.clone(), namespace);
        let patch = serde_json::json!({
            "metadata": {
                "finalizers": [TOPIC_FINALIZER]
            }
        });
        topics
            .patch(
                &topic.name_any(),
                &PatchParams::apply("streamline-operator").force(),
                &Patch::Apply(&patch),
            )
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        Ok(())
    }

    /// Handle deletion: remove topic from Streamline server, then remove finalizer
    async fn handle_deletion(
        &self,
        topic: &StreamlineTopic,
        namespace: &str,
    ) -> std::result::Result<Action, OperatorError> {
        let name = topic.name_any();
        info!(
            "Handling deletion of StreamlineTopic {}/{}",
            namespace, name
        );

        // Attempt to delete topic from the Streamline cluster
        let clusters: Api<StreamlineCluster> = Api::namespaced(self.client.clone(), namespace);
        if let Ok(cluster) = clusters.get(&topic.spec.cluster_ref).await {
            let cluster_name = cluster.name_any();
            let http_endpoint = format!(
                "http://{}-0.{}-headless.{}.svc:{}",
                cluster_name, cluster_name, namespace, cluster.spec.http_port
            );
            info!("Deleting topic {} from cluster at {}", name, http_endpoint);
            if let Err(e) = self
                .http_client
                .delete(format!("{http_endpoint}/api/v1/topics/{name}"))
                .send()
                .await
            {
                warn!("Failed to delete topic from cluster API: {}", e);
            }
        } else {
            warn!(
                "Cluster {} not found during topic deletion, skipping server cleanup",
                topic.spec.cluster_ref
            );
        }

        // Remove finalizer
        let topics: Api<StreamlineTopic> = Api::namespaced(self.client.clone(), namespace);
        let finalizers: Vec<String> = topic
            .metadata
            .finalizers
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|f| f.as_str() != TOPIC_FINALIZER)
            .cloned()
            .collect();

        let patch = serde_json::json!({
            "metadata": {
                "finalizers": finalizers
            }
        });
        topics
            .patch(&name, &PatchParams::default(), &Patch::Merge(&patch))
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        info!(
            "Finalizer removed for StreamlineTopic {}/{}",
            namespace, name
        );
        Ok(Action::await_change())
    }

    /// Create or update a topic in the Streamline cluster.
    ///
    /// Returns the state the server reports for the topic so the status can be
    /// derived from the server's answer rather than from the desired spec.
    async fn create_or_update_topic(
        &self,
        topic: &StreamlineTopic,
        cluster: &StreamlineCluster,
    ) -> Result<TopicServerState> {
        let namespace = cluster.namespace().unwrap_or_else(|| "default".to_string());
        let cluster_name = cluster.name_any();

        // Build the Streamline HTTP API endpoint
        // Use the first broker's HTTP endpoint
        let http_endpoint = format!(
            "http://{}-0.{}-headless.{}.svc:{}",
            cluster_name, cluster_name, namespace, cluster.spec.http_port
        );

        // Build the topic creation request.
        //
        // The core API's `CreateTopicRequest` is `{ name, partitions }` — no
        // replication factor, no config map. Sending more used to make the
        // request *look* like it configured retention/compaction/compression
        // while the server dropped every extra field on the floor. Only the
        // fields the server actually reads are sent; anything else in the spec
        // was already rejected by `unsupported_fields` above.
        let topic_config = serde_json::json!({
            "name": topic.name_any(),
            "partitions": topic.spec.partitions,
        });

        info!(
            "Creating/updating topic {} at {}",
            topic.name_any(),
            http_endpoint,
        );

        let response = self
            .http_client
            .post(format!("{http_endpoint}/api/v1/topics"))
            .json(&topic_config)
            .send()
            .await
            .map_err(|e| {
                OperatorError::Internal(format!(
                    "HTTP request to create topic {} failed: {}",
                    topic.name_any(),
                    e
                ))
            })?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();

        // 409 Conflict means the topic already exists — idempotent, but the
        // conflict body says nothing about the existing topic, so read it back.
        if !status.is_success() && status.as_u16() != 409 {
            return Err(OperatorError::Internal(format!(
                "Failed to create topic {} (HTTP {}): {}",
                topic.name_any(),
                status,
                body
            )));
        }

        let state = parse_topic_state(&body);
        if state.is_empty() {
            return Ok(self
                .fetch_topic_state(&http_endpoint, &topic.name_any())
                .await);
        }

        Ok(state)
    }

    /// Read a topic back from the server, returning an empty state when the
    /// server does not expose (or does not answer) the read endpoint.
    async fn fetch_topic_state(&self, http_endpoint: &str, name: &str) -> TopicServerState {
        match self
            .http_client
            .get(format!("{http_endpoint}/api/v1/topics/{name}"))
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => {
                let body = response.text().await.unwrap_or_default();
                parse_topic_state(&body)
            }
            Ok(response) => {
                warn!(
                    "Server did not report state for topic {} (HTTP {})",
                    name,
                    response.status()
                );
                TopicServerState::default()
            }
            Err(e) => {
                warn!("Failed to read back topic {} from the server: {}", name, e);
                TopicServerState::default()
            }
        }
    }

    /// Publish status derived from the state the server reported.
    async fn update_status_from_server(
        &self,
        topic: &StreamlineTopic,
        namespace: &str,
        state: TopicServerState,
    ) -> Result<()> {
        match evaluate_sync(&topic.spec, state) {
            SyncOutcome::InSync(state) => {
                self.update_status_ready(topic, namespace, state, true)
                    .await
            }
            SyncOutcome::Unverified => {
                warn!(
                    "Server accepted topic {}/{} without reporting its configuration",
                    namespace,
                    topic.name_any()
                );
                self.update_status_ready(topic, namespace, state, false)
                    .await
            }
            SyncOutcome::Drift(message) => {
                error!(
                    "Topic {}/{} does not match the requested spec: {}",
                    namespace,
                    topic.name_any(),
                    message
                );
                self.update_status_error(topic, namespace, &message).await
            }
        }
    }

    /// Publish an Unsupported status for a spec the server cannot honour.
    async fn update_status_unsupported(
        &self,
        topic: &StreamlineTopic,
        namespace: &str,
        message: &str,
    ) -> Result<()> {
        let status = Self::unsupported_status(topic, message);
        self.patch_status_if_changed(topic, namespace, status).await
    }

    /// Patch the topic status, but only when it differs from what is already
    /// published.
    ///
    /// A status patch produces a watch event for the same object. Patching an
    /// unchanged status therefore re-enters `reconcile` immediately, which
    /// patches again — an unbounded hot loop that also rewrites `lastUpdated`
    /// and every condition timestamp on each pass. The status builders keep the
    /// existing timestamps for unchanged data so this comparison can actually
    /// succeed.
    async fn patch_status_if_changed(
        &self,
        topic: &StreamlineTopic,
        namespace: &str,
        status: TopicStatus,
    ) -> Result<()> {
        if topic.status.as_ref() == Some(&status) {
            return Ok(());
        }

        let topics: Api<StreamlineTopic> = Api::namespaced(self.client.clone(), namespace);
        topics
            .patch_status(
                &topic.name_any(),
                &PatchParams::default(),
                &Patch::Merge(&serde_json::json!({ "status": status })),
            )
            .await
            .map_err(|e| OperatorError::KubeApi(e.to_string()))?;

        Ok(())
    }

    /// Stamp `lastUpdated` only when the rest of the status actually changed.
    ///
    /// `lastUpdated` is excluded from the comparison: it is derived from the
    /// clock rather than from topic state, so including it would make every
    /// status differ from the previous one and defeat the skip in
    /// [`Self::patch_status_if_changed`].
    fn with_stable_timestamp(topic: &StreamlineTopic, mut status: TopicStatus) -> TopicStatus {
        let semantic_change = topic.status.as_ref().is_none_or(|current| {
            let mut comparable = current.clone();
            comparable.last_updated = None;
            comparable != status
        });

        status.last_updated = if semantic_change {
            Some(Utc::now().to_rfc3339())
        } else {
            topic
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
    fn seeded_conditions(topic: &StreamlineTopic) -> Vec<ConditionFields> {
        topic
            .status
            .as_ref()
            .map(|status| {
                status
                    .conditions
                    .iter()
                    .map(|condition| ConditionFields {
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

    /// Build the status published for a spec the server cannot honour.
    ///
    /// Pure function of the current object so the fail-closed path can be
    /// asserted, including its stability across repeated reconciles, without a
    /// Kubernetes API server.
    fn unsupported_status(topic: &StreamlineTopic, message: &str) -> TopicStatus {
        let mut cond_fields = Self::seeded_conditions(topic);
        set_condition(
            &mut cond_fields,
            build_condition(
                TOPIC_CONDITION_READY,
                CONDITION_FALSE,
                "UnsupportedConfiguration",
                message,
            ),
        );
        set_condition(
            &mut cond_fields,
            build_condition(
                TOPIC_CONDITION_SYNCED,
                CONDITION_FALSE,
                "UnsupportedConfiguration",
                "The topic was not created because the spec requests unsupported settings",
            ),
        );

        Self::with_stable_timestamp(
            topic,
            TopicStatus {
                ready: false,
                phase: TopicPhase::Failed,
                partitions: 0,
                replication_factor: 0,
                partition_assignments: vec![],
                conditions: cond_fields
                    .into_iter()
                    .map(|c| c.into_topic_condition())
                    .collect(),
                observed_generation: topic.metadata.generation,
                last_updated: None,
                error_message: Some(message.to_string()),
            },
        )
    }

    /// Update status from the server-reported topic state.
    ///
    /// `verified` is false when the server accepted the request but reported no
    /// configuration to compare against; the Synced condition then says
    /// `Unknown` rather than claiming the topic matches the spec.
    async fn update_status_ready(
        &self,
        topic: &StreamlineTopic,
        namespace: &str,
        state: TopicServerState,
        verified: bool,
    ) -> Result<()> {
        let status = Self::ready_status(topic, state, verified);
        self.patch_status_if_changed(topic, namespace, status).await
    }

    /// Build the status published for a topic the server accepted.
    ///
    /// Pure function of the object plus the server-reported state so the
    /// healthy and in-sync paths — and their stability across repeated
    /// reconciles — can be asserted without a Kubernetes API server.
    fn ready_status(
        topic: &StreamlineTopic,
        state: TopicServerState,
        verified: bool,
    ) -> TopicStatus {
        let mut cond_fields = Self::seeded_conditions(topic);
        set_condition(
            &mut cond_fields,
            build_condition(
                TOPIC_CONDITION_READY,
                CONDITION_TRUE,
                "TopicReady",
                "Topic successfully created/updated",
            ),
        );
        set_condition(
            &mut cond_fields,
            if verified {
                build_condition(
                    TOPIC_CONDITION_SYNCED,
                    CONDITION_TRUE,
                    "ConfigSynced",
                    "Server-reported partition count and replication factor match the desired \
                     state. No other topic configuration is applied: the Streamline topic API \
                     accepts only a name and a partition count",
                )
            } else {
                build_condition(
                    TOPIC_CONDITION_SYNCED,
                    CONDITION_UNKNOWN,
                    "ServerDidNotReportConfiguration",
                    "The server accepted the topic but did not report both its partition count \
                     and replication factor, so the desired state could not be verified",
                )
            },
        );

        Self::with_stable_timestamp(
            topic,
            TopicStatus {
                ready: true,
                phase: TopicPhase::Ready,
                // Report what the server said, not what was asked for.
                partitions: state.partitions.unwrap_or_default(),
                replication_factor: state.replication_factor.unwrap_or_default(),
                partition_assignments: vec![],
                conditions: cond_fields
                    .into_iter()
                    .map(|c| c.into_topic_condition())
                    .collect(),
                observed_generation: topic.metadata.generation,
                last_updated: None,
                error_message: None,
            },
        )
    }

    /// Update status to pending
    async fn update_status_pending(
        &self,
        topic: &StreamlineTopic,
        namespace: &str,
        message: &str,
    ) -> Result<()> {
        let status = Self::pending_status(topic, message);
        self.patch_status_if_changed(topic, namespace, status).await
    }

    /// Build the status published while waiting for the referenced cluster.
    fn pending_status(topic: &StreamlineTopic, message: &str) -> TopicStatus {
        let mut cond_fields = Self::seeded_conditions(topic);
        set_condition(
            &mut cond_fields,
            build_condition(TOPIC_CONDITION_READY, CONDITION_FALSE, "Pending", message),
        );
        set_condition(
            &mut cond_fields,
            build_condition(
                TOPIC_CONDITION_SYNCED,
                CONDITION_FALSE,
                "WaitingForCluster",
                "Topic cannot sync until cluster is ready",
            ),
        );

        Self::with_stable_timestamp(
            topic,
            TopicStatus {
                ready: false,
                phase: TopicPhase::Pending,
                partitions: 0,
                replication_factor: 0,
                partition_assignments: vec![],
                conditions: cond_fields
                    .into_iter()
                    .map(|c| c.into_topic_condition())
                    .collect(),
                observed_generation: topic.metadata.generation,
                last_updated: None,
                error_message: None,
            },
        )
    }

    /// Update status to error
    async fn update_status_error(
        &self,
        topic: &StreamlineTopic,
        namespace: &str,
        error_message: &str,
    ) -> Result<()> {
        let status = Self::error_status(topic, error_message);
        self.patch_status_if_changed(topic, namespace, status).await
    }

    /// Build the status published when reconciliation failed or drifted.
    fn error_status(topic: &StreamlineTopic, error_message: &str) -> TopicStatus {
        let mut cond_fields = Self::seeded_conditions(topic);
        set_condition(
            &mut cond_fields,
            build_condition(
                TOPIC_CONDITION_READY,
                CONDITION_FALSE,
                "Error",
                error_message,
            ),
        );
        set_condition(
            &mut cond_fields,
            build_condition(
                TOPIC_CONDITION_SYNCED,
                CONDITION_FALSE,
                "SyncFailed",
                error_message,
            ),
        );

        Self::with_stable_timestamp(
            topic,
            TopicStatus {
                ready: false,
                phase: TopicPhase::Failed,
                partitions: 0,
                replication_factor: 0,
                partition_assignments: vec![],
                conditions: cond_fields
                    .into_iter()
                    .map(|c| c.into_topic_condition())
                    .collect(),
                observed_generation: topic.metadata.generation,
                last_updated: None,
                error_message: Some(error_message.to_string()),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    // unwrap/expect are acceptable in tests; the crate-wide lint targets production code.
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::crd::TopicSpec;

    fn spec(json: &str) -> TopicSpec {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn default_spec_is_supported() {
        assert!(unsupported_fields(&spec(r#"{"clusterRef": "c"}"#)).is_empty());
    }

    #[test]
    fn replication_factor_above_one_is_rejected() {
        let unsupported =
            unsupported_fields(&spec(r#"{"clusterRef": "c", "replicationFactor": 3}"#));
        assert_eq!(unsupported.len(), 1);
        assert!(unsupported[0].contains("replicationFactor=3"));
    }

    #[test]
    fn any_min_insync_replicas_is_rejected() {
        // Even `1` is a request the server cannot honour: it applies no topic
        // config at all, so accepting the field would imply an ISR guarantee
        // that is never configured.
        for value in [1, 2, 3] {
            let unsupported = unsupported_fields(&spec(&format!(
                r#"{{"clusterRef": "c", "config": {{"minInsyncReplicas": {value}}}}}"#
            )));
            assert!(
                unsupported
                    .iter()
                    .any(|m| m.contains("config.minInsyncReplicas")),
                "minInsyncReplicas={value} must be rejected"
            );
        }
    }

    // --- Settings the core API silently discards --------------------------
    //
    // `POST /api/v1/topics` deserialises into `{ name, partitions }`. Every
    // other field the operator used to send was dropped by the server while
    // the operator reported `Ready`/`Synced`, promising retention, compaction
    // and compression the broker never applied.

    #[test]
    fn non_default_retention_is_rejected_before_any_api_call() {
        for (field, json) in [
            (
                "retention.retentionMs",
                r#"{"clusterRef": "c", "retention": {"retentionMs": 3600000}}"#,
            ),
            (
                "retention.retentionBytes",
                r#"{"clusterRef": "c", "retention": {"retentionBytes": 1073741824}}"#,
            ),
            (
                "retention.cleanupPolicy",
                r#"{"clusterRef": "c", "retention": {"cleanupPolicy": "compact"}}"#,
            ),
        ] {
            let unsupported = unsupported_fields(&spec(json));
            assert!(
                unsupported.iter().any(|m| m.contains(field)),
                "{field} must be rejected, got {unsupported:?}"
            );
        }
    }

    #[test]
    fn non_default_compression_is_rejected() {
        let unsupported = unsupported_fields(&spec(
            r#"{"clusterRef": "c", "compression": {"type": "lz4"}}"#,
        ));
        assert!(unsupported.iter().any(|m| m.contains("compression.type")));
    }

    #[test]
    fn every_topic_config_override_is_rejected() {
        for (field, json) in [
            (
                "config.maxMessageBytes",
                r#"{"clusterRef": "c", "config": {"maxMessageBytes": 1048576}}"#,
            ),
            (
                "config.segmentBytes",
                r#"{"clusterRef": "c", "config": {"segmentBytes": 1048576}}"#,
            ),
            (
                "config.indexIntervalBytes",
                r#"{"clusterRef": "c", "config": {"indexIntervalBytes": 4096}}"#,
            ),
            (
                "config.flushIntervalMs",
                r#"{"clusterRef": "c", "config": {"flushIntervalMs": 1000}}"#,
            ),
            (
                "config.flushMessages",
                r#"{"clusterRef": "c", "config": {"flushMessages": 10000}}"#,
            ),
            (
                "config.custom[min.cleanable.dirty.ratio]",
                r#"{"clusterRef": "c", "config": {"custom": {"min.cleanable.dirty.ratio": "0.5"}}}"#,
            ),
        ] {
            let unsupported = unsupported_fields(&spec(json));
            assert!(
                unsupported.iter().any(|m| m.contains(field)),
                "{field} must be rejected, got {unsupported:?}"
            );
        }
    }

    /// The rejection message has to say *why*, or an operator sees a Failed
    /// topic with no idea that the field is inert rather than malformed.
    #[test]
    fn rejection_messages_explain_that_the_server_applies_no_config() {
        let unsupported = unsupported_fields(&spec(
            r#"{"clusterRef": "c", "retention": {"cleanupPolicy": "compact"}}"#,
        ));
        let message = unsupported.join("; ");
        assert!(message.contains("only name and partitions"), "{message}");
        assert!(message.contains("silently discarded"), "{message}");
    }

    /// Explicitly restating a schema default is not a request for anything, so
    /// it must not be rejected — otherwise a round-tripped resource (which
    /// serialises every field) could never be applied.
    #[test]
    fn explicitly_specified_defaults_are_accepted() {
        let defaults = crate::crd::TopicSpec::config_defaults();
        let json = format!(
            r#"{{"clusterRef": "c", "partitions": 3, "replicationFactor": 1,
                 "retention": {{"retentionMs": {}, "retentionBytes": {}, "cleanupPolicy": "{}"}},
                 "compression": {{"type": "{}"}}}}"#,
            defaults.retention_ms,
            defaults.retention_bytes,
            defaults.cleanup_policy,
            defaults.compression_type,
        );
        assert!(unsupported_fields(&spec(&json)).is_empty());
    }

    #[test]
    fn non_positive_partition_counts_are_rejected() {
        let unsupported = unsupported_fields(&spec(r#"{"clusterRef": "c", "partitions": 0}"#));
        assert!(unsupported
            .iter()
            .any(|m| m.contains("partitions must be >= 1")));
    }

    #[test]
    fn server_state_is_parsed_from_flat_and_wrapped_bodies() {
        assert_eq!(
            parse_topic_state(r#"{"partitions": 6, "replication_factor": 1}"#),
            TopicServerState {
                partitions: Some(6),
                replication_factor: Some(1),
            }
        );
        assert_eq!(
            parse_topic_state(r#"{"topic": {"partitionCount": 2}}"#),
            TopicServerState {
                partitions: Some(2),
                replication_factor: None,
            }
        );
    }

    #[test]
    fn server_state_uses_partition_count_when_partitions_is_an_array() {
        // This mirrors the core server's TopicDetails response: `partitions`
        // is a detail array and the numeric count lives in `partition_count`.
        assert_eq!(
            parse_topic_state(
                r#"{
                    "name": "events",
                    "partition_count": 3,
                    "partitions": [{"id": 0}, {"id": 1}, {"id": 2}],
                    "replication_factor": 1
                }"#
            ),
            TopicServerState {
                partitions: Some(3),
                replication_factor: Some(1),
            }
        );
    }

    #[test]
    fn unparseable_or_empty_bodies_report_nothing() {
        assert_eq!(parse_topic_state(""), TopicServerState::default());
        assert_eq!(parse_topic_state("not json"), TopicServerState::default());
        assert_eq!(parse_topic_state("{}"), TopicServerState::default());
    }

    /// The exact body core returns from `POST /api/v1/topics` (its
    /// `TopicDetails`): `config` is empty on create, `replication_factor` is
    /// hard-coded to 1, and `partitions` is a detail array.
    fn core_create_response(name: &str, partitions: i32) -> String {
        let details: Vec<serde_json::Value> = (0..partitions)
            .map(|p| {
                serde_json::json!({
                    "partition_id": p,
                    "leader": 1,
                    "replicas": [1],
                    "isr": [1],
                    "start_offset": 0,
                    "end_offset": 0,
                    "size_bytes": 0,
                })
            })
            .collect();
        serde_json::json!({
            "name": name,
            "partition_count": partitions,
            "replication_factor": 1,
            "is_internal": false,
            "partitions": details,
            "config": {},
            "total_messages": 0,
            "total_bytes": 0,
            "messages_per_second": 0.0,
            "bytes_per_second": 0.0,
        })
        .to_string()
    }

    /// The body core returns from `GET /api/v1/topics/{name}`: `config` holds
    /// the **server's** defaults, which have nothing to do with the CRD's.
    fn core_get_response(name: &str, partitions: i32) -> String {
        let mut body: serde_json::Value =
            serde_json::from_str(&core_create_response(name, partitions)).unwrap();
        body["config"] = serde_json::json!({
            "retention.ms": "-1",
            "retention.bytes": "-1",
            "segment.bytes": "104857600",
            "cleanup.policy": "delete",
            "message.ttl.ms": "-1",
        });
        body.to_string()
    }

    #[test]
    fn core_create_response_is_parsed_into_verifiable_state() {
        assert_eq!(
            parse_topic_state(&core_create_response("events", 3)),
            TopicServerState {
                partitions: Some(3),
                replication_factor: Some(1),
            }
        );
    }

    #[test]
    fn core_get_response_is_parsed_into_verifiable_state() {
        assert_eq!(
            parse_topic_state(&core_get_response("events", 6)),
            TopicServerState {
                partitions: Some(6),
                replication_factor: Some(1),
            }
        );
    }

    /// The server's `config` map reports the *server's* settings, not the
    /// operator's request, so it must never be folded into the sync verdict.
    ///
    /// The CRD default and the server now agree on unlimited retention, so
    /// equality alone can no longer prove anything here. The fixture instead
    /// makes the server report a retention and a cleanup policy the spec never
    /// asked for, and shows neither reaches the parsed state or the status.
    #[test]
    fn the_servers_own_config_defaults_are_not_treated_as_spec_agreement() {
        let requested = spec(r#"{"clusterRef": "c", "partitions": 6}"#);
        assert!(
            core_get_response("events", 6).contains(r#""retention.ms":"-1""#),
            "fixture must carry the server's own retention default"
        );
        assert_eq!(
            requested.retention.retention_ms, -1,
            "the CRD default now states what the server actually does"
        );

        // Report config that contradicts the spec outright: if the map were
        // ever consulted, this is what would leak into the status.
        let mut body: serde_json::Value =
            serde_json::from_str(&core_get_response("events", 6)).unwrap();
        body["config"]["retention.ms"] = serde_json::json!("604800000");
        body["config"]["cleanup.policy"] = serde_json::json!("compact");
        let body = body.to_string();

        let state = parse_topic_state(&body);
        // Only partitions and replication factor are carried forward, so no
        // retention claim of any kind can reach the status.
        assert_eq!(state.partitions, Some(6));
        assert_eq!(state.replication_factor, Some(1));

        let status = TopicController::ready_status(&topic(r#"{"clusterRef": "c"}"#), state, true);
        let rendered = serde_json::to_string(&status).unwrap();
        assert!(
            !rendered.contains("604800000") && !rendered.contains("compact"),
            "the server's config map must not reach the status: {rendered}"
        );

        let synced = status
            .conditions
            .iter()
            .find(|c| c.r#type == TOPIC_CONDITION_SYNCED)
            .expect("Synced condition");
        let message = synced.message.clone().unwrap_or_default();
        assert!(
            message.contains("partition count and replication factor"),
            "the Synced message must scope its claim: {message}"
        );
        assert!(
            message.contains("No other topic configuration is applied"),
            "the Synced message must disclaim unapplied config: {message}"
        );
    }

    /// Characterization: the operator never claims a seven-day retention
    /// policy, on any path.
    ///
    /// The CRD used to default `spec.retention.retentionMs` to `604800000`
    /// while the core topic API applied no topic configuration at all. Nothing
    /// deleted anything after seven days, but `kubectl explain`, the generated
    /// schema, and every server-defaulted resource said otherwise — and
    /// because the controller rejects non-default values, seven days was also
    /// the only value a user was allowed to keep. This pins both halves of the
    /// fix: the default now says "unlimited", and explicitly asking for the old
    /// seven-day window is refused rather than accepted and dropped.
    #[test]
    fn the_operator_never_claims_a_seven_day_retention_policy() {
        const SEVEN_DAYS_MS: i64 = 604_800_000;

        // 1. A default spec asks for nothing, is accepted, and is unlimited.
        let default_spec = spec(r#"{"clusterRef": "c"}"#);
        assert_eq!(
            default_spec.retention.retention_ms, -1,
            "the default must describe the broker's actual behaviour"
        );
        assert_ne!(default_spec.retention.retention_ms, SEVEN_DAYS_MS);
        assert!(
            unsupported_fields(&default_spec).is_empty(),
            "a default spec requests nothing the server cannot do"
        );

        // 2. Nothing the operator publishes for it claims a seven-day window.
        let state = parse_topic_state(&core_create_response("events", 3));
        let status = TopicController::ready_status(&topic(r#"{"clusterRef": "c"}"#), state, true);
        let rendered = serde_json::to_string(&status).unwrap().to_ascii_lowercase();
        for claim in ["604800000", "7 days", "seven days", "7-day", "seven-day"] {
            assert!(
                !rendered.contains(claim),
                "the published status claims `{claim}`, a retention the server never \
                 applies: {rendered}"
            );
        }

        // 3. Explicitly asking for the old default is still fail-closed: the
        //    server applies no retention, so accepting it would re-create
        //    exactly the promise this fix removed.
        let explicit = spec(&format!(
            r#"{{"clusterRef": "c", "retention": {{"retentionMs": {SEVEN_DAYS_MS}}}}}"#
        ));
        let unsupported = unsupported_fields(&explicit);
        let message = unsupported.join("; ");
        assert!(
            message.contains("retention.retentionMs"),
            "an explicit seven-day retention must be rejected, got {unsupported:?}"
        );
        assert!(
            message.contains(&SEVEN_DAYS_MS.to_string()),
            "the rejection must quote what was asked for: {message}"
        );
        assert!(
            message.contains("leave it at the default -1"),
            "the rejection must point at the unlimited default: {message}"
        );
    }

    /// The three v0.3.0 topic defaults must each be rejected *with the patch
    /// that clears them*, or a user reading a `Failed` status learns only that
    /// something is wrong.
    #[test]
    fn v0_3_0_topic_defaults_are_rejected_with_their_patch() {
        for (field, json) in [
            (
                "spec.retention.retentionMs",
                r#"{"clusterRef": "c", "retention": {"retentionMs": 604800000}}"#,
            ),
            (
                "spec.config.minInsyncReplicas",
                r#"{"clusterRef": "c", "config": {"minInsyncReplicas": 1}}"#,
            ),
            (
                "spec.config.maxMessageBytes",
                r#"{"clusterRef": "c", "config": {"maxMessageBytes": 1048576}}"#,
            ),
        ] {
            let entry = crate::upgrade::legacy_default(field)
                .unwrap_or_else(|| panic!("{field} must be in the upgrade table"));
            let unsupported = unsupported_fields(&spec(json));
            let message = unsupported.join("; ");

            assert!(
                message.contains(&entry.supported_yaml()),
                "{field}: the rejection must name the value to write: {message}"
            );
            assert!(
                message.contains(entry.merge_patch),
                "{field}: the rejection must carry the patch: {message}"
            );
            assert!(
                message.contains(crate::upgrade::UPGRADE_GUIDE),
                "{field}: the rejection must cite the guide: {message}"
            );
            assert!(
                message.contains("v0.3.0") && message.contains("persisted"),
                "{field}: the stored value came from the old schema; say so: {message}"
            );
        }
    }

    /// Remediation must not turn an explicit, deliberate value into a message
    /// that blames an upgrade — and must not go missing either.
    ///
    /// "Blaming" is the provenance claim (*the API server persisted this into
    /// your resource*), not any mention of v0.3.0: a removal has to state that
    /// the old CRDs default the key back, because that is true whoever wrote
    /// the value.
    #[test]
    fn a_deliberate_topic_value_is_rejected_without_blaming_the_upgrade() {
        let message = unsupported_fields(&spec(
            r#"{"clusterRef": "c", "retention": {"retentionMs": 3600000},
                "config": {"minInsyncReplicas": 3}}"#,
        ))
        .join("; ");

        assert!(
            message.contains("retention.retentionMs=3600000"),
            "{message}"
        );
        assert!(message.contains("config.minInsyncReplicas=3"), "{message}");
        for blame in [
            "persisted into every",
            "without anyone having written it",
            "does not rewrite specs",
        ] {
            assert!(
                !message.contains(blame),
                "neither value came from an upgrade, so `{blame}` is false here: {message}"
            );
        }
        assert!(
            message.contains("spec.retention.retentionMs: -1")
                && message.contains("spec.config.minInsyncReplicas: null"),
            "a deliberate value still needs an actionable fix: {message}"
        );
    }

    /// Fields that were never v0.3.0 defaults must keep the plain message: an
    /// upgrade patch appended to `segmentBytes` would be an invented fix.
    #[test]
    fn settings_that_were_never_legacy_defaults_get_no_upgrade_patch() {
        for json in [
            r#"{"clusterRef": "c", "config": {"segmentBytes": 1048576}}"#,
            r#"{"clusterRef": "c", "compression": {"type": "lz4"}}"#,
            r#"{"clusterRef": "c", "retention": {"cleanupPolicy": "compact"}}"#,
        ] {
            let message = unsupported_fields(&spec(json)).join("; ");
            assert!(!message.is_empty(), "{json} must still be rejected");
            assert!(
                !message.contains("kubectl patch"),
                "no upgrade patch exists for this field: {message}"
            );
            assert!(
                !message.contains(crate::upgrade::UPGRADE_GUIDE),
                "the upgrade guide does not cover this field: {message}"
            );
        }
    }

    #[test]
    fn matching_server_state_is_in_sync() {
        let spec = spec(r#"{"clusterRef": "c", "partitions": 3}"#);
        let state = parse_topic_state(&core_create_response("events", 3));
        assert_eq!(evaluate_sync(&spec, state), SyncOutcome::InSync(state));
    }

    /// A half-answer is not agreement: claiming `Synced=True` from a partial
    /// response asserts a replication factor the server never reported.
    #[test]
    fn a_partially_reported_topic_is_unverified_not_in_sync() {
        let spec = spec(r#"{"clusterRef": "c", "partitions": 3}"#);

        assert_eq!(
            evaluate_sync(
                &spec,
                TopicServerState {
                    partitions: Some(3),
                    replication_factor: None,
                }
            ),
            SyncOutcome::Unverified
        );
        assert_eq!(
            evaluate_sync(
                &spec,
                TopicServerState {
                    partitions: None,
                    replication_factor: Some(1),
                }
            ),
            SyncOutcome::Unverified
        );
    }

    /// Drift still wins over an incomplete answer: a wrong partition count is
    /// an error even when the replication factor is missing.
    #[test]
    fn partial_but_conflicting_state_is_drift() {
        let spec = spec(r#"{"clusterRef": "c", "partitions": 6}"#);
        let state = TopicServerState {
            partitions: Some(1),
            replication_factor: None,
        };
        assert!(matches!(evaluate_sync(&spec, state), SyncOutcome::Drift(_)));
    }

    #[test]
    fn mismatched_server_state_is_drift_not_ready() {
        let spec = spec(r#"{"clusterRef": "c", "partitions": 6}"#);
        let state = TopicServerState {
            partitions: Some(1),
            replication_factor: None,
        };
        match evaluate_sync(&spec, state) {
            SyncOutcome::Drift(message) => {
                assert!(message.contains("1 partitions"));
                assert!(message.contains("6"));
            }
            other => panic!("expected drift, got {other:?}"),
        }
    }

    #[test]
    fn silent_server_leaves_sync_unverified() {
        let spec = spec(r#"{"clusterRef": "c"}"#);
        assert_eq!(
            evaluate_sync(&spec, TopicServerState::default()),
            SyncOutcome::Unverified
        );
    }

    // --- Status stability -------------------------------------------------
    //
    // A status patch generates a watch event for the same object. If the
    // controller rebuilds `lastUpdated` and every condition timestamp on each
    // pass, the patch is never a no-op and the controller re-triggers itself
    // forever. These tests pin the fail-closed and in-sync paths to a status
    // that compares equal when nothing changed.

    fn topic(json: &str) -> StreamlineTopic {
        let mut topic = StreamlineTopic::new("events", spec(json));
        topic.metadata.namespace = Some("streamline".to_string());
        topic.metadata.generation = Some(1);
        topic
    }

    #[test]
    fn repeated_unsupported_status_is_stable() {
        let mut resource = topic(r#"{"clusterRef": "c", "replicationFactor": 3}"#);
        let message = "replicationFactor=3 is not supported";

        let first = TopicController::unsupported_status(&resource, message);
        assert_eq!(first.phase, TopicPhase::Failed);
        assert!(!first.ready);
        resource.status = Some(first.clone());

        let second = TopicController::unsupported_status(&resource, message);
        assert_eq!(
            second, first,
            "an unchanged unsupported spec must produce an identical status"
        );
    }

    #[test]
    fn repeated_in_sync_status_is_stable() {
        let mut resource = topic(r#"{"clusterRef": "c", "partitions": 3}"#);
        let state = TopicServerState {
            partitions: Some(3),
            replication_factor: Some(1),
        };

        let first = TopicController::ready_status(&resource, state, true);
        assert!(first.ready);
        assert_eq!(first.phase, TopicPhase::Ready);
        assert_eq!(first.partitions, 3);
        resource.status = Some(first.clone());

        let second = TopicController::ready_status(&resource, state, true);
        assert_eq!(
            second, first,
            "an unchanged in-sync topic must produce an identical status"
        );
    }

    #[test]
    fn repeated_pending_and_error_statuses_are_stable() {
        let mut resource = topic(r#"{"clusterRef": "c"}"#);

        let pending = TopicController::pending_status(&resource, "Waiting for cluster to be ready");
        resource.status = Some(pending.clone());
        assert_eq!(
            TopicController::pending_status(&resource, "Waiting for cluster to be ready"),
            pending
        );

        let mut errored = topic(r#"{"clusterRef": "c"}"#);
        let failure = TopicController::error_status(&errored, "Cluster c not found");
        errored.status = Some(failure.clone());
        assert_eq!(
            TopicController::error_status(&errored, "Cluster c not found"),
            failure
        );
    }

    #[test]
    fn becoming_ready_refreshes_the_timestamp_and_transitions_only_what_changed() {
        let mut resource = topic(r#"{"clusterRef": "c", "partitions": 3}"#);
        let mut pending = TopicController::pending_status(&resource, "Waiting for cluster");
        pending.last_updated = Some("2024-01-01T00:00:00Z".to_string());
        resource.status = Some(pending);

        let state = TopicServerState {
            partitions: Some(3),
            replication_factor: Some(1),
        };
        let ready = TopicController::ready_status(&resource, state, true);

        assert!(ready.ready);
        assert_ne!(
            ready.last_updated.as_deref(),
            Some("2024-01-01T00:00:00Z"),
            "a real state change must refresh lastUpdated"
        );
        let ready_condition = ready
            .conditions
            .iter()
            .find(|c| c.r#type == TOPIC_CONDITION_READY)
            .expect("Ready condition");
        assert_eq!(ready_condition.status, CONDITION_TRUE);
    }

    #[test]
    fn status_only_differing_in_timestamp_is_not_treated_as_a_change() {
        let mut resource = topic(r#"{"clusterRef": "c", "partitions": 3}"#);
        let state = TopicServerState {
            partitions: Some(3),
            replication_factor: Some(1),
        };
        let mut published = TopicController::ready_status(&resource, state, true);
        published.last_updated = Some("2024-01-01T00:00:00Z".to_string());
        resource.status = Some(published.clone());

        let next = TopicController::ready_status(&resource, state, true);
        assert_eq!(
            next.last_updated.as_deref(),
            Some("2024-01-01T00:00:00Z"),
            "lastUpdated must be preserved when nothing else changed"
        );
        assert_eq!(next, published);
    }

    /// An unverified sync must not be able to masquerade as a verified one just
    /// because the previous status said `Synced=True`.
    #[test]
    fn losing_server_verification_changes_the_synced_condition() {
        let mut resource = topic(r#"{"clusterRef": "c", "partitions": 3}"#);
        let state = TopicServerState {
            partitions: Some(3),
            replication_factor: Some(1),
        };
        resource.status = Some(TopicController::ready_status(&resource, state, true));

        let unverified = TopicController::ready_status(&resource, state, false);
        let synced = unverified
            .conditions
            .iter()
            .find(|c| c.r#type == TOPIC_CONDITION_SYNCED)
            .expect("Synced condition");
        assert_eq!(synced.status, CONDITION_UNKNOWN);
        assert_ne!(unverified, resource.status.expect("previous status"));
    }
}
