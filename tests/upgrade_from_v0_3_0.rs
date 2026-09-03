//! Characterization tests for the v0.3.0 → current upgrade.
//!
//! v0.3.0 shipped CRDs whose structural-schema defaults the API server wrote
//! into every stored object it reached:
//! `StreamlineCluster.spec.replicas: 3`,
//! `StreamlineCluster.spec.podAntiAffinity: true`,
//! `StreamlineTopic.spec.replicationFactor: 2`,
//! `StreamlineTopic.spec.retention.retentionMs: 604800000`,
//! `StreamlineTopic.spec.config.minInsyncReplicas: 1`, and
//! `StreamlineTopic.spec.config.maxMessageBytes: 1048576`. This operator
//! rejects all six and does not mutate user specs, so an installation that
//! upgrades without patching sees its resources stop reconciling.
//!
//! That is the correct behaviour — three independent brokers pretending to be a
//! quorum, or a topic advertising a seven-day retention nothing enforces, is
//! worse than a `Failed` status — but it is only *safe* if the rejection tells
//! the reader what to do. These tests pin both halves:
//!
//! 1. A stored resource in the v0.3.0 shape is still rejected, for exactly the
//!    expected reasons, with messages that name the field, the value to set (or
//!    the key to delete), the namespaced `kubectl patch`, and
//!    `docs/UPGRADING.md`.
//! 2. Applying the merge patch the documentation prints — as an RFC 7386 merge
//!    patch, which is what `kubectl patch --type merge` sends — turns each
//!    rejected resource into an accepted one, and leaves every sibling setting
//!    that was not being corrected exactly where it was.
//! 3. The documentation names every old value, new value, and patch command in
//!    `streamline_operator::upgrade::LEGACY_DEFAULTS`, so a reworded rejection
//!    cannot drift away from the guide it cites, and it tells the truth about
//!    the fields v0.3.0 advertised that no longer exist.
//!
//! Hermetic: no cluster, no network, no Docker.

// unwrap/expect are acceptable in tests; the crate-wide lint targets production code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use streamline_operator::upgrade::{
    LegacyDefault, DOCS_NAMESPACE, LEGACY_CLUSTER_REPLICAS, LEGACY_DEFAULTS,
    LEGACY_POD_ANTI_AFFINITY, LEGACY_TOPIC_MAX_MESSAGE_BYTES, LEGACY_TOPIC_MIN_INSYNC_REPLICAS,
    LEGACY_TOPIC_REPLICATION_FACTOR, LEGACY_TOPIC_RETENTION_MS, UPGRADE_GUIDE,
};
use streamline_operator::{StreamlineCluster, StreamlineTopic, TopicController};

fn read(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {relative}: {e}"))
}

/// A `StreamlineCluster` as v0.3.0 stored it.
///
/// The spec is written the way `kubectl get -o yaml` returns it, with the
/// defaults the API server materialised on write — including the two this
/// operator rejects, and the `replication` block v0.3.0's hand-written CRD
/// accepted but no v0.3.0 code path ever read. Everything else is a value this
/// operator still accepts, so a failure here is about the upgrade and nothing
/// else.
const V0_3_0_CLUSTER: &str = r#"
apiVersion: streamline.io/v1alpha1
kind: StreamlineCluster
metadata:
  name: my-cluster
  namespace: streamline-system
  generation: 1
spec:
  replicas: 3
  image: ghcr.io/streamlinelabs/streamline:0.3.0
  imagePullPolicy: IfNotPresent
  podAntiAffinity: true
  kafkaPort: 9092
  httpPort: 9094
  raftPort: 9093
  logLevel: info
  metricsEnabled: true
  nodeSelector: {}
  env: []
  tolerations: []
  storage:
    size: 10Gi
  replication:
    enabled: false
    mode: active-passive
    maxLagMs: 5000
status:
  phase: Running
  readyReplicas: 3
"#;

/// A `StreamlineTopic` as v0.3.0 stored it when the manifest named neither
/// `retention:` nor `config:`.
///
/// Structural defaulting only descends into objects that are present, and
/// v0.3.0 gave neither block an object-level default, so a topic like this
/// carries exactly one rejected value. Keeping it separate from
/// [`V0_3_0_TOPIC_WITH_BLOCKS`] is the point: the guide's discovery queries
/// have to find both shapes, and only one of them needs the nested patches.
const V0_3_0_TOPIC: &str = r#"
apiVersion: streamline.io/v1alpha1
kind: StreamlineTopic
metadata:
  name: events
  namespace: streamline-system
  generation: 1
spec:
  clusterRef: my-cluster
  partitions: 3
  replicationFactor: 2
status:
  phase: Ready
  ready: true
"#;

/// A `StreamlineTopic` as v0.3.0 stored it when the manifest opened the
/// `retention:` and `config:` blocks — which is all it took, including
/// `config: {}`.
///
/// `flushMs` is here on purpose: v0.3.0's hand-written CRD advertised it while
/// the v0.3.0 Rust type only ever had `flushIntervalMs`, so the field was
/// stored and ignored. It must stay ignored — and must not be mistaken for the
/// supported `flushIntervalMs` — after the upgrade.
const V0_3_0_TOPIC_WITH_BLOCKS: &str = r#"
apiVersion: streamline.io/v1alpha1
kind: StreamlineTopic
metadata:
  name: orders
  namespace: streamline-system
  generation: 1
spec:
  clusterRef: my-cluster
  partitions: 6
  replicationFactor: 2
  retention:
    retentionMs: 604800000
    retentionBytes: -1
    cleanupPolicy: delete
  config:
    minInsyncReplicas: 1
    maxMessageBytes: 1048576
    flushMs: 1000
  compression:
    type: producer
status:
  phase: Ready
  ready: true
"#;

/// Every fixture, for the guards that must hold across all of them.
const FIXTURES: &[(&str, &str)] = &[
    ("V0_3_0_CLUSTER", V0_3_0_CLUSTER),
    ("V0_3_0_TOPIC", V0_3_0_TOPIC),
    ("V0_3_0_TOPIC_WITH_BLOCKS", V0_3_0_TOPIC_WITH_BLOCKS),
];

fn legacy(field: &str) -> &'static LegacyDefault {
    LEGACY_DEFAULTS
        .iter()
        .find(|entry| entry.field == field)
        .unwrap_or_else(|| panic!("{field} must be in the upgrade table"))
}

/// Apply a documented merge patch to a resource exactly as the API server would
/// apply what `kubectl patch --type merge` sends (RFC 7386).
fn apply_merge_patch<T: serde::de::DeserializeOwned + serde::Serialize>(
    resource: &T,
    merge_patch: &str,
) -> T {
    let mut document = serde_json::to_value(resource).expect("resource serializes");
    let patch: serde_json::Value =
        serde_json::from_str(merge_patch).expect("the documented patch is JSON");
    json_patch::merge(&mut document, &patch);
    serde_json::from_value(document).expect("the patched resource still deserializes")
}

/// Every rejection must be actionable on its own: the field, the target value,
/// the patch, and the guide.
fn assert_message_is_actionable(message: &str, entry: &LegacyDefault) {
    assert!(
        message.contains(&entry.supported_yaml()),
        "the rejection must name the value to set ({}): {message}",
        entry.supported_yaml()
    );
    assert!(
        message.contains(entry.merge_patch),
        "the rejection must carry the patch body ({}): {message}",
        entry.merge_patch
    );
    assert!(
        message.contains(&format!("kubectl patch {}", entry.resource)),
        "the rejection must name the resource to patch: {message}"
    );
    assert!(
        message.contains("--type merge"),
        "the rejection must name the patch type the guide uses: {message}"
    );
    assert!(
        message.contains(UPGRADE_GUIDE),
        "the rejection must point at {UPGRADE_GUIDE}: {message}"
    );
}

// ---------------------------------------------------------------------------
// The stored v0.3.0 shapes are still rejected, for the right reasons
// ---------------------------------------------------------------------------

#[test]
fn a_v0_3_0_cluster_is_rejected_for_replicas_and_pod_anti_affinity() {
    let cluster: StreamlineCluster =
        serde_yaml::from_str(V0_3_0_CLUSTER).expect("a v0.3.0 cluster still deserializes");

    assert_eq!(cluster.spec.replicas, LEGACY_CLUSTER_REPLICAS);
    assert_eq!(cluster.spec.pod_anti_affinity, LEGACY_POD_ANTI_AFFINITY);

    let errors = cluster
        .spec
        .validate()
        .expect_err("a v0.3.0 cluster must not be accepted");

    // Exactly the two upgrade blockers: if a third appears, the guide is
    // incomplete and following it would leave the resource broken.
    assert_eq!(
        errors.len(),
        2,
        "expected only the two v0.3.0 defaults to be rejected, got: {errors:#?}"
    );

    let replicas = errors
        .iter()
        .find(|e| e.starts_with("replicas=3 is not supported"))
        .unwrap_or_else(|| panic!("replicas=3 must be rejected by name: {errors:#?}"));
    assert_message_is_actionable(replicas, legacy("spec.replicas"));

    let affinity = errors
        .iter()
        .find(|e| e.starts_with("podAntiAffinity is not supported"))
        .unwrap_or_else(|| panic!("podAntiAffinity must be rejected by name: {errors:#?}"));
    assert_message_is_actionable(affinity, legacy("spec.podAntiAffinity"));
}

#[test]
fn a_v0_3_0_topic_is_rejected_for_replication_factor() {
    let topic: StreamlineTopic =
        serde_yaml::from_str(V0_3_0_TOPIC).expect("a v0.3.0 topic still deserializes");

    assert_eq!(
        topic.spec.replication_factor,
        LEGACY_TOPIC_REPLICATION_FACTOR
    );

    let rejected = TopicController::unsupported_fields(&topic.spec);
    assert_eq!(
        rejected.len(),
        1,
        "a topic that never opened retention/config blocks carries one legacy value, \
         got: {rejected:#?}"
    );
    assert!(
        rejected[0].starts_with("replicationFactor=2 is not supported"),
        "the rejection must name the stored value: {}",
        rejected[0]
    );
    assert_message_is_actionable(&rejected[0], legacy("spec.replicationFactor"));
}

/// A topic whose manifest opened `retention:` and `config:` picked up three
/// more defaults. All four must be rejected, each with its own patch: a user
/// who fixes only `replicationFactor` because that is all the message named
/// would still be stuck.
#[test]
fn a_v0_3_0_topic_with_blocks_is_rejected_for_all_four_defaults() {
    let topic: StreamlineTopic =
        serde_yaml::from_str(V0_3_0_TOPIC_WITH_BLOCKS).expect("a v0.3.0 topic still deserializes");

    assert_eq!(topic.spec.retention.retention_ms, LEGACY_TOPIC_RETENTION_MS);
    assert_eq!(
        topic.spec.config.min_insync_replicas,
        Some(LEGACY_TOPIC_MIN_INSYNC_REPLICAS)
    );
    assert_eq!(
        topic.spec.config.max_message_bytes,
        Some(LEGACY_TOPIC_MAX_MESSAGE_BYTES)
    );

    let rejected = TopicController::unsupported_fields(&topic.spec);
    assert_eq!(
        rejected.len(),
        4,
        "expected exactly the four v0.3.0 topic defaults, got: {rejected:#?}"
    );

    for (field, prefix) in [
        ("spec.replicationFactor", "replicationFactor=2"),
        (
            "spec.retention.retentionMs",
            "retention.retentionMs=604800000",
        ),
        (
            "spec.config.minInsyncReplicas",
            "config.minInsyncReplicas=1",
        ),
        (
            "spec.config.maxMessageBytes",
            "config.maxMessageBytes=1048576",
        ),
    ] {
        let message = rejected
            .iter()
            .find(|m| m.starts_with(prefix))
            .unwrap_or_else(|| panic!("{prefix} must be rejected by name: {rejected:#?}"));
        assert_message_is_actionable(message, legacy(field));
    }
}

/// `flushMs` was in v0.3.0's CRD but never in its Rust type, and it is gone
/// from both now. It must not be silently re-read as `flushIntervalMs`, and it
/// must not block the upgrade — the API server drops it on the next write.
#[test]
fn the_removed_flush_ms_field_is_inert_and_blocks_nothing() {
    assert!(
        V0_3_0_TOPIC_WITH_BLOCKS.contains("flushMs: 1000"),
        "the fixture must still exercise the removed field"
    );

    let topic: StreamlineTopic = serde_yaml::from_str(V0_3_0_TOPIC_WITH_BLOCKS).unwrap();
    assert_eq!(
        topic.spec.config.flush_interval_ms, None,
        "flushMs must not be read as flushIntervalMs: they were never the same field"
    );
    assert!(
        !TopicController::unsupported_fields(&topic.spec)
            .iter()
            .any(|m| m.contains("flushMs")),
        "an inert field the API server prunes must not be reported as a blocker"
    );
}

/// `spec.replication` was advertised by v0.3.0's hand-written CRD and read by
/// nothing. Removing it from the schema must not turn a stored cluster into an
/// invalid one.
#[test]
fn the_removed_cluster_replication_block_is_inert_and_blocks_nothing() {
    assert!(
        V0_3_0_CLUSTER.contains("replication:"),
        "the fixture must still exercise the removed block"
    );

    let cluster: StreamlineCluster = serde_yaml::from_str(V0_3_0_CLUSTER).unwrap();
    let serialized = serde_json::to_string(&cluster.spec).unwrap();
    assert!(
        !serialized.contains("replication"),
        "spec.replication must not round-trip through a type that has no such field: {serialized}"
    );

    let errors = cluster.spec.validate().unwrap_err();
    assert!(
        !errors.iter().any(|e| e.contains("replication")),
        "an inert block the API server prunes must not be reported as a blocker: {errors:#?}"
    );
}

/// Both rejections must say the value was *persisted* rather than chosen, or
/// the reader goes looking for a manifest that does not contain it.
#[test]
fn rejections_explain_that_the_api_server_persisted_the_value() {
    let cluster: StreamlineCluster = serde_yaml::from_str(V0_3_0_CLUSTER).unwrap();
    let topic: StreamlineTopic = serde_yaml::from_str(V0_3_0_TOPIC_WITH_BLOCKS).unwrap();

    let mut messages = cluster.spec.validate().unwrap_err();
    messages.extend(TopicController::unsupported_fields(&topic.spec));

    for message in &messages {
        assert!(
            message.contains("v0.3.0"),
            "a legacy default must be named as one: {message}"
        );
        assert!(
            message.contains("persisted"),
            "the reader must learn the value came from the API server: {message}"
        );
        assert!(
            message.contains("does not rewrite specs"),
            "the reader must learn the operator will not fix it for them: {message}"
        );
    }
}

/// A nested default only reached etcd when the surrounding block existed, so
/// the rejection must say that instead of claiming every topic has it. A user
/// told "every StreamlineTopic carries this" goes looking for topics that never
/// did and stops believing the message.
#[test]
fn nested_rejections_do_not_claim_every_resource_carries_the_value() {
    let topic: StreamlineTopic = serde_yaml::from_str(V0_3_0_TOPIC_WITH_BLOCKS).unwrap();
    let messages = TopicController::unsupported_fields(&topic.spec);

    for (prefix, parent) in [
        ("retention.retentionMs=", "spec.retention"),
        ("config.minInsyncReplicas=", "spec.config"),
        ("config.maxMessageBytes=", "spec.config"),
    ] {
        let message = messages
            .iter()
            .find(|m| m.starts_with(prefix))
            .unwrap_or_else(|| panic!("{prefix} must be rejected: {messages:#?}"));
        assert!(
            message.contains(&format!("`{parent}` block")),
            "the rejection must name the block that had to exist: {message}"
        );
        assert!(
            !message.contains("created against those CRDs"),
            "that wording claims every resource carries it, which is false here: {message}"
        );
    }
}

/// A value a user typed is not an upgrade artefact, and must not be blamed on
/// one — but it still needs the same remediation.
#[test]
fn a_deliberate_value_is_rejected_without_blaming_the_upgrade() {
    let mut cluster: StreamlineCluster = serde_yaml::from_str(V0_3_0_CLUSTER).unwrap();
    cluster.spec.replicas = 5;
    cluster.spec.pod_anti_affinity = false;

    let errors = cluster.spec.validate().unwrap_err();
    let replicas = errors
        .iter()
        .find(|e| e.starts_with("replicas=5 is not supported"))
        .unwrap_or_else(|| panic!("replicas=5 must still be rejected: {errors:#?}"));

    assert!(
        !replicas.contains("v0.3.0"),
        "replicas=5 was never a v0.3.0 default: {replicas}"
    );
    assert_message_is_actionable(replicas, legacy("spec.replicas"));
}

// ---------------------------------------------------------------------------
// The documented patches actually clear the rejections
// ---------------------------------------------------------------------------

#[test]
fn the_documented_cluster_patches_make_a_v0_3_0_cluster_valid() {
    let mut cluster: StreamlineCluster = serde_yaml::from_str(V0_3_0_CLUSTER).unwrap();

    for field in ["spec.replicas", "spec.podAntiAffinity"] {
        cluster = apply_merge_patch(&cluster, legacy(field).merge_patch);
    }

    assert_eq!(cluster.spec.replicas, 1);
    assert!(!cluster.spec.pod_anti_affinity);
    assert!(
        cluster.spec.validate().is_ok(),
        "the documented patches must leave a cluster the operator accepts: {:?}",
        cluster.spec.validate().unwrap_err()
    );

    // The patch is surgical: nothing else in the stored object moves.
    assert_eq!(
        cluster.spec.image,
        "ghcr.io/streamlinelabs/streamline:0.3.0"
    );
    assert_eq!(cluster.spec.kafka_port, 9092);
    assert_eq!(cluster.spec.storage.size, "10Gi");
    assert_eq!(cluster.metadata.name.as_deref(), Some("my-cluster"));
    assert_eq!(cluster.metadata.namespace.as_deref(), Some(DOCS_NAMESPACE));
}

#[test]
fn the_documented_topic_patch_makes_a_v0_3_0_topic_valid() {
    let topic: StreamlineTopic = serde_yaml::from_str(V0_3_0_TOPIC).unwrap();
    let patched: StreamlineTopic =
        apply_merge_patch(&topic, legacy("spec.replicationFactor").merge_patch);

    assert_eq!(patched.spec.replication_factor, 1);
    assert!(
        TopicController::unsupported_fields(&patched.spec).is_empty(),
        "the documented patch must leave a topic the operator accepts: {:?}",
        TopicController::unsupported_fields(&patched.spec)
    );
    assert_eq!(patched.spec.partitions, 3, "partitions must be untouched");
    assert_eq!(patched.spec.cluster_ref, "my-cluster");
}

/// All four documented topic patches, applied to the shape that carries all
/// four defaults. Each one has to be surgical: the retention patch must not
/// disturb `cleanupPolicy`, and the two `spec.config` removals must not take
/// the block — or each other — with them.
#[test]
fn the_documented_topic_patches_make_a_v0_3_0_topic_with_blocks_valid() {
    let mut topic: StreamlineTopic = serde_yaml::from_str(V0_3_0_TOPIC_WITH_BLOCKS).unwrap();

    for field in [
        "spec.replicationFactor",
        "spec.retention.retentionMs",
        "spec.config.minInsyncReplicas",
        "spec.config.maxMessageBytes",
    ] {
        topic = apply_merge_patch(&topic, legacy(field).merge_patch);
    }

    assert_eq!(topic.spec.replication_factor, 1);
    assert_eq!(topic.spec.retention.retention_ms, -1);
    assert_eq!(topic.spec.config.min_insync_replicas, None);
    assert_eq!(topic.spec.config.max_message_bytes, None);
    assert!(
        TopicController::unsupported_fields(&topic.spec).is_empty(),
        "the documented patches must leave a topic the operator accepts: {:?}",
        TopicController::unsupported_fields(&topic.spec)
    );

    // Siblings the patches never named are untouched.
    assert_eq!(topic.spec.partitions, 6);
    assert_eq!(topic.spec.cluster_ref, "my-cluster");
    assert_eq!(topic.spec.retention.retention_bytes, -1);
    assert_eq!(topic.spec.retention.cleanup_policy, "delete");
    assert_eq!(topic.spec.compression.r#type, "producer");
    assert_eq!(topic.metadata.name.as_deref(), Some("orders"));
    assert_eq!(topic.metadata.namespace.as_deref(), Some(DOCS_NAMESPACE));
}

/// The blunt fix — `{"spec":{"config":null}}` — would clear the rejections too,
/// and would throw away every other setting in the block while doing it. This
/// pins that the documented patch removes one leaf and nothing else, checked on
/// the raw JSON so a field the Rust type does not model still counts.
#[test]
fn a_config_removal_takes_the_leaf_and_leaves_the_block() {
    let stored: serde_json::Value = serde_yaml::from_str(V0_3_0_TOPIC_WITH_BLOCKS).unwrap();
    let mut document = stored.clone();
    let patch: serde_json::Value =
        serde_json::from_str(legacy("spec.config.minInsyncReplicas").merge_patch).unwrap();
    json_patch::merge(&mut document, &patch);

    let config = document["spec"]["config"]
        .as_object()
        .expect("spec.config must survive the removal of one of its keys");

    assert!(
        !config.contains_key("minInsyncReplicas"),
        "the patched key must be gone: {config:#?}"
    );
    assert_eq!(
        config
            .get("maxMessageBytes")
            .and_then(serde_json::Value::as_i64),
        Some(LEGACY_TOPIC_MAX_MESSAGE_BYTES),
        "the sibling this patch did not name must survive: {config:#?}"
    );
    assert_eq!(
        config.get("flushMs").and_then(serde_json::Value::as_i64),
        Some(1000),
        "a setting the operator does not model must not be collateral damage: {config:#?}"
    );
    assert_eq!(
        document["spec"]["retention"], stored["spec"]["retention"],
        "a spec.config patch must not touch spec.retention"
    );
    assert_eq!(document["spec"]["partitions"], stored["spec"]["partitions"]);
}

/// The retention patch sets a value rather than deleting the key, because
/// deleting it while the v0.3.0 CRDs are installed restores the seven-day
/// default on the same write. Pin the direction so nobody "simplifies" it into
/// a removal.
#[test]
fn the_retention_patch_sets_a_value_and_is_not_a_removal() {
    let entry = legacy("spec.retention.retentionMs");

    assert!(
        !entry.removes_the_key(),
        "removing retentionMs under the v0.3.0 CRDs re-defaults it to 604800000"
    );
    assert!(
        !entry.needs_corrected_crds_first(),
        "an explicit -1 is valid under both schemas, so it needs no ordering"
    );
    assert_eq!(entry.supported_value, "-1");

    let stored: serde_json::Value = serde_yaml::from_str(V0_3_0_TOPIC_WITH_BLOCKS).unwrap();
    let mut document = stored.clone();
    let patch: serde_json::Value = serde_json::from_str(entry.merge_patch).unwrap();
    json_patch::merge(&mut document, &patch);

    assert_eq!(document["spec"]["retention"]["retentionMs"], -1);
    assert_eq!(
        document["spec"]["retention"]["cleanupPolicy"],
        stored["spec"]["retention"]["cleanupPolicy"],
        "the retention patch must not disturb its siblings"
    );
    assert_eq!(
        document["spec"]["retention"]["retentionBytes"],
        stored["spec"]["retention"]["retentionBytes"]
    );
}

/// The two removals only stick once the corrected CRDs are installed, so the
/// guide has to say so where a reader will meet it.
#[test]
fn the_guide_orders_the_crd_install_before_the_key_removals() {
    let guide = read(UPGRADE_GUIDE);
    let removals: Vec<&LegacyDefault> = LEGACY_DEFAULTS
        .iter()
        .filter(|entry| entry.needs_corrected_crds_first())
        .collect();

    assert!(
        !removals.is_empty(),
        "this test is meaningless if nothing needs the ordering"
    );

    let install = guide
        .find("kubectl apply -k deploy/crds/")
        .expect("the guide must install the corrected CRDs");

    for entry in removals {
        let patch = guide
            .find(entry.merge_patch)
            .unwrap_or_else(|| panic!("{UPGRADE_GUIDE} must show the patch for {}", entry.field));
        assert!(
            install < patch,
            "{UPGRADE_GUIDE} must install the corrected CRDs before removing {}: \
             v0.3.0 defaults that key back on the same write",
            entry.field
        );
    }

    assert!(
        guide.contains("re-defaults") || guide.contains("re-materialise"),
        "{UPGRADE_GUIDE} must explain why the order matters, not just assert it"
    );
}

/// Validation is a read: rejecting a legacy value must never quietly correct
/// the spec the user can see.
#[test]
fn rejecting_a_legacy_value_does_not_mutate_the_spec() {
    let cluster: StreamlineCluster = serde_yaml::from_str(V0_3_0_CLUSTER).unwrap();
    let before = serde_json::to_string(&cluster.spec).unwrap();
    let _ = cluster.spec.validate();
    assert_eq!(
        before,
        serde_json::to_string(&cluster.spec).unwrap(),
        "validation must not rewrite the cluster spec"
    );

    let topic: StreamlineTopic = serde_yaml::from_str(V0_3_0_TOPIC).unwrap();
    let before = serde_json::to_string(&topic.spec).unwrap();
    let _ = TopicController::unsupported_fields(&topic.spec);
    assert_eq!(
        before,
        serde_json::to_string(&topic.spec).unwrap(),
        "the topic gate must not rewrite the topic spec"
    );
}

// ---------------------------------------------------------------------------
// The upgrade documentation says what the rejections promise it says
// ---------------------------------------------------------------------------

/// Sources that must carry the upgrade path, in the order a user meets them.
const UPGRADE_SOURCES: &[&str] = &[UPGRADE_GUIDE, "README.md", "CHANGELOG.md"];

#[test]
fn the_upgrade_guide_exists_and_is_reachable_from_the_front_door() {
    let readme = read("README.md");
    assert!(
        readme.contains(UPGRADE_GUIDE),
        "README must link {UPGRADE_GUIDE}"
    );

    // "Prominent" is not a matter of taste here: a user who reads only the top
    // of the README must still be told to patch.
    let position = readme
        .find(UPGRADE_GUIDE)
        .expect("README must link the upgrade guide");
    let architecture = readme
        .find("## Architecture")
        .expect("README must keep its Architecture section");
    assert!(
        position < architecture,
        "the upgrade notice must appear before the README's first regular section"
    );

    assert!(
        read("CHANGELOG.md").contains(UPGRADE_GUIDE),
        "CHANGELOG must link {UPGRADE_GUIDE}"
    );
}

#[test]
fn every_source_names_every_old_and_new_value() {
    for source in UPGRADE_SOURCES {
        let body = read(source);
        for entry in LEGACY_DEFAULTS {
            assert!(
                body.contains(&entry.legacy_yaml()),
                "{source} must name the value v0.3.0 persisted ({})",
                entry.legacy_yaml()
            );
            assert!(
                body.contains(&entry.supported_yaml()),
                "{source} must name the value to set ({})",
                entry.supported_yaml()
            );
        }
    }
}

#[test]
fn every_source_carries_the_exact_patch_command_for_each_field() {
    for source in UPGRADE_SOURCES {
        let body = read(source);
        for entry in LEGACY_DEFAULTS {
            let command = entry.patch_command("my-resource", DOCS_NAMESPACE);
            let prefix = format!("kubectl patch {}", entry.resource);
            let suffix = format!("--type merge -p '{}'", entry.merge_patch);

            assert!(
                body.contains(&prefix),
                "{source} must show `{prefix}` (documented form: {command})"
            );
            assert!(
                body.contains(&suffix),
                "{source} must show the exact patch `{suffix}`"
            );
        }
    }
}

/// A patch without a namespace runs against whatever `kubectl` defaults to,
/// silently succeeding against nothing when the resource lives elsewhere.
///
/// Only lines that actually name a resource are commands; prose that mentions
/// `kubectl patch --type merge` in passing is not an instruction to run.
#[test]
fn every_documented_patch_command_is_namespace_aware() {
    let mut offenders: Vec<String> = Vec::new();

    for source in UPGRADE_SOURCES {
        for (index, line) in read(source).lines().enumerate() {
            let is_command = LEGACY_DEFAULTS
                .iter()
                .any(|entry| line.contains(&format!("kubectl patch {}", entry.resource)));
            if !is_command {
                continue;
            }
            if line.contains("-n ") || line.contains("--namespace") {
                continue;
            }
            offenders.push(format!("{source}:{}: {}", index + 1, line.trim()));
        }
    }

    assert!(
        offenders.is_empty(),
        "documented patch commands must name a namespace:\n  {}",
        offenders.join("\n  ")
    );
}

/// The check above is only meaningful if it would catch a namespace-less
/// command, so prove it does rather than trusting an empty result.
#[test]
fn the_namespace_check_rejects_a_command_without_one() {
    let namespaced = "kubectl patch streamlineclusters my-cluster -n streamline-system \
                      --type merge -p '{\"spec\":{\"replicas\":1}}'";
    let bare = "kubectl patch streamlineclusters my-cluster --type merge \
                -p '{\"spec\":{\"replicas\":1}}'";

    let names_a_resource = |line: &str| {
        LEGACY_DEFAULTS
            .iter()
            .any(|entry| line.contains(&format!("kubectl patch {}", entry.resource)))
    };
    let is_namespaced = |line: &str| line.contains("-n ") || line.contains("--namespace");

    assert!(names_a_resource(namespaced) && is_namespaced(namespaced));
    assert!(
        names_a_resource(bare) && !is_namespaced(bare),
        "the check must flag a patch that omits the namespace"
    );
}

/// The guide's own examples must target the namespace the shipped operator
/// watches; `deploy/` is the source of truth, and `tests/docs_examples.rs`
/// derives the same value independently.
#[test]
fn the_guide_patches_the_namespace_the_operator_watches() {
    let guide = read(UPGRADE_GUIDE);
    assert!(
        guide.contains(&format!("-n {DOCS_NAMESPACE}")),
        "{UPGRADE_GUIDE} must patch in {DOCS_NAMESPACE}"
    );
    assert!(
        read("deploy/operator.yaml").contains(&format!("namespace: {DOCS_NAMESPACE}")),
        "{DOCS_NAMESPACE} must be the namespace deploy/ installs into"
    );
}

/// Patching what you can see is only half the job: a user must be able to find
/// every affected resource, rehearse the change, and check it landed.
#[test]
fn the_guide_documents_discovery_dry_run_and_verification() {
    let guide = read(UPGRADE_GUIDE);

    for required in [
        // Discovery — v0.3.0 ignored --namespace, so this must be cluster-wide.
        "kubectl get streamlineclusters --all-namespaces",
        "kubectl get streamlinetopics --all-namespaces",
        // Rehearsal against the real API server, without persisting.
        "--dry-run=server",
        // Verification after the fact.
        "custom-columns=",
        ".status.phase",
    ] {
        assert!(
            guide.contains(required),
            "{UPGRADE_GUIDE} must document `{required}`"
        );
    }

    for entry in LEGACY_DEFAULTS {
        assert!(
            guide.contains(&format!(".{}", entry.field)),
            "{UPGRADE_GUIDE} must show how to query .{}",
            entry.field
        );
    }

    // A nested default was only stored where the block existed, so a plain
    // "not equal to the new default" filter would skip topics that never had
    // the field — which is correct — and the guide has to say which query finds
    // which shape rather than leaving the reader to guess.
    for entry in LEGACY_DEFAULTS.iter().filter(|e| e.parent.is_some()) {
        assert!(
            guide.contains(&format!("{}=", entry.key))
                || guide.contains(&format!("{}:", entry.key)),
            "{UPGRADE_GUIDE} must print {} in its discovery output",
            entry.key
        );
    }
}

/// The release notes must stay under `[Unreleased]`: no version has been chosen
/// for this fix, and filing it under an invented one would tell users to look
/// for a release that does not exist.
#[test]
fn the_changelog_upgrade_notes_live_under_unreleased() {
    let changelog = read("CHANGELOG.md");

    let unreleased = changelog
        .find("## [Unreleased]")
        .expect("CHANGELOG must have an Unreleased section");
    let notes = changelog
        .find("Upgrading from 0.3.0")
        .expect("CHANGELOG must carry the upgrade notes");
    let next_release = changelog[unreleased + 1..]
        .find("\n## [")
        .map(|offset| unreleased + 1 + offset)
        .expect("CHANGELOG must contain a released section after Unreleased");

    assert!(
        unreleased < notes && notes < next_release,
        "the upgrade notes must sit inside [Unreleased], not under a released version"
    );
}

/// The guide quotes the rejection a reader will actually see. Quoting a
/// message the operator no longer publishes would send them looking for text
/// that does not exist, so the sample is checked against the real one with
/// whitespace collapsed (the guide wraps it to fit).
#[test]
fn the_guide_quotes_the_message_the_operator_actually_publishes() {
    fn collapse(text: &str) -> String {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    let cluster: StreamlineCluster = serde_yaml::from_str(V0_3_0_CLUSTER).unwrap();
    let errors = cluster.spec.validate().unwrap_err();
    let published = errors
        .iter()
        .find(|e| e.starts_with("replicas=3 is not supported"))
        .expect("the replicas rejection");

    assert!(
        collapse(&read(UPGRADE_GUIDE)).contains(&collapse(published)),
        "{UPGRADE_GUIDE} must quote the message the operator publishes:\n{published}"
    );
}

/// A guard on the guards: if the fixtures ever stop describing v0.3.0, every
/// assertion above would pass while testing nothing.
#[test]
fn the_fixtures_still_carry_the_values_this_upgrade_is_about() {
    for entry in LEGACY_DEFAULTS {
        let stored = FIXTURES
            .iter()
            .any(|(_, body)| body.contains(&format!("{}: {}", entry.key, entry.legacy_value)));

        assert!(
            stored,
            "no fixture stores {}: {} — the upgrade tests for {} are testing nothing",
            entry.key, entry.legacy_value, entry.field
        );
    }
}

// ---------------------------------------------------------------------------
// The guide tells the truth about the fields that no longer exist
// ---------------------------------------------------------------------------

/// Fields v0.3.0's hand-written CRDs advertised and this tree does not ship.
///
/// Neither was ever read by v0.3.0's Rust types — `spec.replication` had no
/// `ClusterSpec` field at all, and `flushMs` never matched `flushIntervalMs` —
/// so removing them changes no behaviour. What it does change is what the API
/// server stores: once the corrected CRDs are installed, a structural schema
/// prunes unknown fields on the next write, and the stored values are gone.
/// Saying "field names are unchanged" would hide that.
const REMOVED_FIELDS: &[(&str, &str)] = &[
    ("StreamlineCluster.spec.replication", "streamlineclusters"),
    ("StreamlineTopic.spec.config.flushMs", "streamlinetopics"),
];

#[test]
fn the_guide_documents_the_fields_that_no_longer_exist() {
    let guide = read(UPGRADE_GUIDE);

    for (field, _) in REMOVED_FIELDS {
        assert!(
            guide.contains(field),
            "{UPGRADE_GUIDE} must name the removed field `{field}`"
        );
    }

    assert!(
        guide.contains("flushIntervalMs"),
        "{UPGRADE_GUIDE} must name the field that replaced flushMs"
    );
    assert!(
        guide.contains("prun"),
        "{UPGRADE_GUIDE} must explain that unknown fields are pruned"
    );
}

/// The guide used to claim the schemas were compatible because "field names are
/// unchanged". Two of them changed. A rollback plan built on that sentence
/// silently loses whatever those fields held.
#[test]
fn the_guide_does_not_claim_the_field_names_are_unchanged() {
    let guide = read(UPGRADE_GUIDE);

    for stale in [
        "field names are unchanged",
        "field names) are unchanged",
        "and field names are unchanged",
    ] {
        assert!(
            !guide.contains(stale),
            "{UPGRADE_GUIDE} still claims `{stale}`, which {} and {} contradict",
            REMOVED_FIELDS[0].0,
            REMOVED_FIELDS[1].0
        );
    }
}

/// Rolling back is not symmetric: reinstalling the v0.3.0 CRDs re-defaults
/// every leaf the upgrade removed, and pruned fields do not come back. Both
/// have to be stated where someone reaches for a rollback.
#[test]
fn the_guide_states_what_rolling_back_does_not_restore() {
    let guide = read(UPGRADE_GUIDE);
    let rollback = guide
        .find("## Rolling back")
        .expect("{UPGRADE_GUIDE} must keep a rollback section");
    let section = &guide[rollback..];

    for required in ["prun", "re-default"] {
        assert!(
            section.contains(required),
            "the rollback section must mention `{required}`: reinstalling v0.3.0 restores the \
             old defaults and cannot restore pruned fields"
        );
    }

    for entry in LEGACY_DEFAULTS.iter().filter(|e| e.removes_the_key()) {
        assert!(
            section.contains(entry.key),
            "the rollback section must say what happens to {} — v0.3.0 defaults it back",
            entry.field
        );
    }
}
