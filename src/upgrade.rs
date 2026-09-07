//! Upgrade metadata for the incompatible defaults shipped in v0.3.0.
//!
//! # Why this module exists
//!
//! The v0.3.0 CRDs declared schema defaults that this operator refuses to
//! render:
//!
//! | Field | v0.3.0 default | Supported value |
//! |-------|----------------|-----------------|
//! | `StreamlineCluster.spec.replicas` | `3` | `1` |
//! | `StreamlineCluster.spec.podAntiAffinity` | `true` | `false` |
//! | `StreamlineTopic.spec.replicationFactor` | `2` | `1` |
//! | `StreamlineTopic.spec.retention.retentionMs` | `604800000` | `-1` (unlimited) |
//! | `StreamlineTopic.spec.config.minInsyncReplicas` | `1` | removed |
//! | `StreamlineTopic.spec.config.maxMessageBytes` | `1048576` | removed |
//!
//! Structural-schema defaulting is applied by the API server on **write**, so
//! those values were persisted into the stored objects. Deleting the field from
//! a manifest does not remove them — the API server simply re-defaults on the
//! next write until the CRD is replaced, and the stored object keeps the old
//! value in the meantime.
//!
//! The first three are top-level and were injected into *every* resource
//! created against v0.3.0. The last three sit inside `spec.retention` and
//! `spec.config`, and structural defaulting only descends into objects that are
//! actually present: neither block carried an object-level default in v0.3.0,
//! so those three were persisted into every resource whose manifest carried the
//! surrounding block — including an empty `config: {}` — and into no other.
//! [`LegacyDefault::parent`] records that distinction so a rejection never
//! claims a provenance it cannot support.
//!
//! The corrected operator still fails closed on all six: `replicas: 3` renders
//! three *independent* brokers rather than a quorum (nothing bootstraps raft
//! peers), `podAntiAffinity: true` is a placement request the pod template
//! never carries, `replicationFactor: 2` claims durability the single replica
//! topic API does not provide, and the three topic settings describe retention,
//! acknowledgement, and message-size policy that the topic API — which accepts
//! only `{name, partitions}` — never applied. Accepting them would be worse
//! than rejecting them.
//!
//! What the operator must not do is *silently* abandon those resources. Every
//! rejection therefore names the exact field, the value to set, the namespaced
//! `kubectl patch` that sets it, and [`UPGRADE_GUIDE`]. The operator never
//! rewrites a user's spec: a controller that quietly edited `replicas: 3` down
//! to `1` would be changing a durability decision on the user's behalf, in a
//! field they can see, with no record of who did it.
//!
//! This table is the single source of truth shared by the validation messages
//! and by the characterization tests in `tests/upgrade_from_v0_3_0.rs`, which
//! assert the upgrade documentation names every old value, new value, and patch
//! command below.

/// Repository-relative path of the upgrade guide every legacy rejection cites.
pub const UPGRADE_GUIDE: &str = "docs/UPGRADING.md";

/// The namespace the shipped `deploy/` watches, and the one every documented
/// command is written against.
///
/// `deploy/operator.yaml` passes `--namespace=$(OPERATOR_NAMESPACE)` bound to
/// the Deployment's own namespace, so a resource anywhere else is never
/// reconciled — which makes a namespace-less `kubectl patch` in the upgrade
/// guide an instruction to patch the wrong object.
pub const DOCS_NAMESPACE: &str = "streamline-system";

/// `StreamlineCluster.spec.replicas` as persisted by the v0.3.0 schema default.
pub const LEGACY_CLUSTER_REPLICAS: i32 = 3;

/// The only broker count this operator renders without inventing a quorum.
pub const SUPPORTED_CLUSTER_REPLICAS: i32 = 1;

/// `StreamlineCluster.spec.podAntiAffinity` as persisted by v0.3.0.
pub const LEGACY_POD_ANTI_AFFINITY: bool = true;

/// The only value that matches what the pod template actually renders.
pub const SUPPORTED_POD_ANTI_AFFINITY: bool = false;

/// `StreamlineTopic.spec.replicationFactor` as persisted by v0.3.0.
pub const LEGACY_TOPIC_REPLICATION_FACTOR: i32 = 2;

/// The only replication factor the Streamline topic API creates.
pub const SUPPORTED_TOPIC_REPLICATION_FACTOR: i32 = 1;

/// `StreamlineTopic.spec.retention.retentionMs` as persisted by v0.3.0: seven
/// days, a window nothing ever enforced.
pub const LEGACY_TOPIC_RETENTION_MS: i64 = 604_800_000;

/// Unlimited retention — the current schema default, and what the broker
/// actually does, since it applies no topic configuration.
pub const SUPPORTED_TOPIC_RETENTION_MS: i64 = -1;

/// `StreamlineTopic.spec.config.minInsyncReplicas` as persisted by v0.3.0.
pub const LEGACY_TOPIC_MIN_INSYNC_REPLICAS: i32 = 1;

/// `StreamlineTopic.spec.config.maxMessageBytes` as persisted by v0.3.0.
pub const LEGACY_TOPIC_MAX_MESSAGE_BYTES: i64 = 1_048_576;

/// The YAML/JSON literal that deletes a key in an RFC 7386 merge patch.
///
/// `spec.config` entries are optional in the current schema and *every* value
/// is rejected, so there is nothing to set them to: the only correct value is
/// no value. Writing `null` says exactly that in both a manifest (the field
/// deserializes to `None`) and a `kubectl patch --type merge` body (the key is
/// deleted).
pub const REMOVED_VALUE: &str = "null";

/// How a rejected value is corrected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Remediation {
    /// Write [`LegacyDefault::supported_value`] into the field.
    ///
    /// Both schemas accept the new value, so the patch can be applied before or
    /// after the corrected CRDs are installed and survives either way.
    SetValue,
    /// Delete the key with a JSON merge-patch `null`.
    ///
    /// The field is optional with no default in the current schema, but v0.3.0
    /// declared one. Structural defaulting runs *after* the patch is merged, so
    /// while the v0.3.0 CRDs are still installed the API server re-materialises
    /// the key on the very same write and the removal appears to do nothing.
    /// Install the corrected CRDs first; see [`LegacyDefault::needs_corrected_crds_first`].
    RemoveKey,
}

/// One field whose v0.3.0 schema default is rejected by this operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LegacyDefault {
    /// Kind carrying the field, e.g. `StreamlineCluster`.
    pub kind: &'static str,
    /// Plural resource name for `kubectl`, e.g. `streamlineclusters`.
    pub resource: &'static str,
    /// Fully qualified field path, e.g. `spec.replicas`.
    pub field: &'static str,
    /// Leaf key as it appears in a spec block and in rejection messages.
    pub key: &'static str,
    /// The spec block that had to be present for v0.3.0 to inject this
    /// default, or `None` for a top-level field that was always injected.
    ///
    /// Structural defaulting only descends into objects that exist in the
    /// submitted document. `spec.retention` and `spec.config` carried no
    /// object-level default in v0.3.0, so their leaf defaults reached etcd only
    /// for resources whose manifest opened the block.
    pub parent: Option<&'static str>,
    /// The value v0.3.0 persisted, rendered exactly as it appears in YAML.
    pub legacy_value: &'static str,
    /// The value this operator supports, rendered exactly as it appears in
    /// YAML — [`REMOVED_VALUE`] when the fix is to delete the key.
    pub supported_value: &'static str,
    /// Whether the fix sets a value or deletes the key.
    pub remediation: Remediation,
    /// The merge-patch body that moves the field from legacy to supported.
    ///
    /// Always nested to the leaf, never to the block: `{"spec":{"config":null}}`
    /// would discard every sibling setting the user still needs to see.
    pub merge_patch: &'static str,
}

impl LegacyDefault {
    /// The namespaced `kubectl patch` that corrects one named object.
    ///
    /// Namespace is a required argument rather than a default: the shipped
    /// operator watches exactly one namespace, and a patch run against the
    /// wrong one succeeds while leaving the broken object untouched.
    #[must_use]
    pub fn patch_command(&self, name: &str, namespace: &str) -> String {
        format!(
            "kubectl patch {} {name} -n {namespace} --type merge -p '{}'",
            self.resource, self.merge_patch
        )
    }

    /// The patch with placeholders, for messages that have no object in hand.
    #[must_use]
    pub fn patch_template(&self) -> String {
        self.patch_command("<name>", "<namespace>")
    }

    /// `spec.replicas: 3` — how the doc and the tests name the old value.
    #[must_use]
    pub fn legacy_yaml(&self) -> String {
        format!("{}: {}", self.field, self.legacy_value)
    }

    /// `spec.replicas: 1` — how the doc and the tests name the new value.
    ///
    /// For a [`Remediation::RemoveKey`] field this is
    /// `spec.config.minInsyncReplicas: null`, which is literally what both a
    /// manifest and a merge patch have to say to make the field go away.
    #[must_use]
    pub fn supported_yaml(&self) -> String {
        format!("{}: {}", self.field, self.supported_value)
    }

    /// Whether the fix deletes the key instead of setting it.
    #[must_use]
    pub fn removes_the_key(&self) -> bool {
        matches!(self.remediation, Remediation::RemoveKey)
    }

    /// Whether the corrected CRDs must be installed *before* the patch for the
    /// patch to survive the write it is part of.
    ///
    /// True exactly for the key removals: v0.3.0 defaults those leaves, and
    /// defaulting runs after the merge, so the API server puts the key straight
    /// back. Value assignments are accepted by both schemas and need no
    /// ordering.
    #[must_use]
    pub fn needs_corrected_crds_first(&self) -> bool {
        self.removes_the_key()
    }

    /// Whether `observed` — rendered the way the rejection renders it — is the
    /// value v0.3.0 persisted, rather than one the user chose.
    #[must_use]
    pub fn observed_is_legacy(&self, observed: &str) -> bool {
        observed == self.legacy_value
    }

    /// The remediation sentence appended to a rejection.
    ///
    /// `observed_is_legacy` is threaded through rather than assumed: a user who
    /// deliberately wrote `replicas: 5` is not upgrading from anything, and
    /// telling them v0.3.0 put it there would be false. Both forms still carry
    /// the field, the target value, the patch, and the guide, because both
    /// readers need to know what to do next.
    #[must_use]
    pub fn remediation(&self, observed_is_legacy: bool) -> String {
        let provenance = if observed_is_legacy {
            match self.parent {
                None => format!(
                    " It is the v0.3.0 CRD schema default, which the API server persisted into \
                     every {} created against those CRDs, so the spec can carry it without \
                     anyone having written it. The operator does not rewrite specs.",
                    self.kind
                ),
                // Nested defaults are only injected into blocks that exist, so
                // "every resource" would be an overclaim: say what actually
                // happened, or the reader stops trusting the message.
                Some(parent) => format!(
                    " It is the v0.3.0 CRD schema default, which the API server persisted into \
                     every {} whose manifest opened a `{}` block (structural defaults are only \
                     injected into objects that are present), so the spec can carry it without \
                     anyone having written it. The operator does not rewrite specs.",
                    self.kind, parent
                ),
            }
        } else {
            String::new()
        };

        let action = if self.removes_the_key() {
            let parent = self.parent.unwrap_or("spec");
            format!(
                "Remove the key — writing `{}` deletes this one leaf and leaves every other {} \
                 setting in place — and re-apply, or patch the stored object in place with `{}`. \
                 The removal only sticks once the corrected CRDs are installed: v0.3.0 declares a \
                 default for this field, and defaulting runs after the patch is merged, so the \
                 API server puts the key back on the same write.",
                self.supported_yaml(),
                parent,
                self.patch_template()
            )
        } else {
            format!(
                "Set {} and re-apply, or patch the stored object in place with `{}`.",
                self.supported_yaml(),
                self.patch_template()
            )
        };

        format!(
            " {action}{provenance} See {UPGRADE_GUIDE} for the full upgrade path, including how \
             to find every affected resource."
        )
    }
}

/// Every v0.3.0 default this operator rejects, in the order the upgrade guide
/// presents them.
pub const LEGACY_DEFAULTS: &[LegacyDefault] = &[
    LegacyDefault {
        kind: "StreamlineCluster",
        resource: "streamlineclusters",
        field: "spec.replicas",
        key: "replicas",
        parent: None,
        legacy_value: "3",
        supported_value: "1",
        remediation: Remediation::SetValue,
        merge_patch: r#"{"spec":{"replicas":1}}"#,
    },
    LegacyDefault {
        kind: "StreamlineCluster",
        resource: "streamlineclusters",
        field: "spec.podAntiAffinity",
        key: "podAntiAffinity",
        parent: None,
        legacy_value: "true",
        supported_value: "false",
        remediation: Remediation::SetValue,
        merge_patch: r#"{"spec":{"podAntiAffinity":false}}"#,
    },
    LegacyDefault {
        kind: "StreamlineTopic",
        resource: "streamlinetopics",
        field: "spec.replicationFactor",
        key: "replicationFactor",
        parent: None,
        legacy_value: "2",
        supported_value: "1",
        remediation: Remediation::SetValue,
        merge_patch: r#"{"spec":{"replicationFactor":1}}"#,
    },
    // `-1` rather than a removal: the current schema defaults this leaf to
    // `-1`, but v0.3.0 defaults it to 604800000, so deleting the key while the
    // old CRDs are installed restores the seven-day claim. An explicit `-1` is
    // accepted by both schemas (neither constrains the range) and means the
    // same thing under both — unlimited, which is what the broker does.
    LegacyDefault {
        kind: "StreamlineTopic",
        resource: "streamlinetopics",
        field: "spec.retention.retentionMs",
        key: "retentionMs",
        parent: Some("spec.retention"),
        legacy_value: "604800000",
        supported_value: "-1",
        remediation: Remediation::SetValue,
        merge_patch: r#"{"spec":{"retention":{"retentionMs":-1}}}"#,
    },
    // No value is accepted for the two `spec.config` leaves, so the only fix is
    // to delete them. The patch nulls the leaf, not `spec.config`, so a
    // `segmentBytes` or `custom` entry the user still has to deal with is not
    // silently thrown away with it.
    LegacyDefault {
        kind: "StreamlineTopic",
        resource: "streamlinetopics",
        field: "spec.config.minInsyncReplicas",
        key: "minInsyncReplicas",
        parent: Some("spec.config"),
        legacy_value: "1",
        supported_value: REMOVED_VALUE,
        remediation: Remediation::RemoveKey,
        merge_patch: r#"{"spec":{"config":{"minInsyncReplicas":null}}}"#,
    },
    LegacyDefault {
        kind: "StreamlineTopic",
        resource: "streamlinetopics",
        field: "spec.config.maxMessageBytes",
        key: "maxMessageBytes",
        parent: Some("spec.config"),
        legacy_value: "1048576",
        supported_value: REMOVED_VALUE,
        remediation: Remediation::RemoveKey,
        merge_patch: r#"{"spec":{"config":{"maxMessageBytes":null}}}"#,
    },
];

/// Look up one entry by its fully qualified field path.
///
/// Returns `None` rather than panicking so a validation message can never take
/// an operator process down; the callers pass literals that the unit tests
/// below pin, so a typo shows up as a failing test.
#[must_use]
pub fn legacy_default(field: &str) -> Option<&'static LegacyDefault> {
    LEGACY_DEFAULTS.iter().find(|entry| entry.field == field)
}

/// The remediation sentence for `field`, or an empty string if it is not a
/// v0.3.0 default.
///
/// Callers append this to a rejection they have already worded, so an unknown
/// field degrades to the original message instead of an empty or misleading
/// one.
#[must_use]
pub fn remediation_for(field: &str, observed_is_legacy: bool) -> String {
    legacy_default(field)
        .map(|entry| entry.remediation(observed_is_legacy))
        .unwrap_or_default()
}

/// The remediation sentence for `field`, deciding the v0.3.0 provenance from
/// the value the caller is about to reject.
///
/// `observed` is the value rendered exactly as the rejection renders it, so a
/// caller never has to keep its own copy of the legacy constant in step with
/// this table — which is how the two would drift apart.
#[must_use]
pub fn remediation_for_value(field: &str, observed: &str) -> String {
    legacy_default(field)
        .map(|entry| entry.remediation(entry.observed_is_legacy(observed)))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    // unwrap/expect are acceptable in tests; the crate-wide lint targets production code.
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn the_table_matches_the_typed_constants() {
        let replicas = legacy_default("spec.replicas").expect("spec.replicas is tabled");
        assert_eq!(replicas.legacy_value, LEGACY_CLUSTER_REPLICAS.to_string());
        assert_eq!(
            replicas.supported_value,
            SUPPORTED_CLUSTER_REPLICAS.to_string()
        );

        let affinity =
            legacy_default("spec.podAntiAffinity").expect("spec.podAntiAffinity is tabled");
        assert_eq!(affinity.legacy_value, LEGACY_POD_ANTI_AFFINITY.to_string());
        assert_eq!(
            affinity.supported_value,
            SUPPORTED_POD_ANTI_AFFINITY.to_string()
        );

        let replication =
            legacy_default("spec.replicationFactor").expect("spec.replicationFactor is tabled");
        assert_eq!(
            replication.legacy_value,
            LEGACY_TOPIC_REPLICATION_FACTOR.to_string()
        );
        assert_eq!(
            replication.supported_value,
            SUPPORTED_TOPIC_REPLICATION_FACTOR.to_string()
        );

        let retention =
            legacy_default("spec.retention.retentionMs").expect("retentionMs is tabled");
        assert_eq!(
            retention.legacy_value,
            LEGACY_TOPIC_RETENTION_MS.to_string()
        );
        assert_eq!(
            retention.supported_value,
            SUPPORTED_TOPIC_RETENTION_MS.to_string()
        );

        let min_isr =
            legacy_default("spec.config.minInsyncReplicas").expect("minInsyncReplicas is tabled");
        assert_eq!(
            min_isr.legacy_value,
            LEGACY_TOPIC_MIN_INSYNC_REPLICAS.to_string()
        );
        assert_eq!(min_isr.supported_value, REMOVED_VALUE);

        let max_bytes =
            legacy_default("spec.config.maxMessageBytes").expect("maxMessageBytes is tabled");
        assert_eq!(
            max_bytes.legacy_value,
            LEGACY_TOPIC_MAX_MESSAGE_BYTES.to_string()
        );
        assert_eq!(max_bytes.supported_value, REMOVED_VALUE);
    }

    /// The three nested defaults must agree with the schema they are corrected
    /// against, or the guide sends people to patch a field into a value the
    /// operator still rejects.
    #[test]
    fn the_supported_values_are_the_ones_the_current_schema_accepts() {
        let defaults = crate::crd::TopicSpec::config_defaults();
        assert_eq!(
            defaults.retention_ms, SUPPORTED_TOPIC_RETENTION_MS,
            "the guide patches retentionMs to the current schema default"
        );

        let spec: crate::crd::TopicSpec =
            serde_json::from_str(r#"{"clusterRef": "c"}"#).expect("a minimal topic deserializes");
        assert!(
            spec.config.min_insync_replicas.is_none() && spec.config.max_message_bytes.is_none(),
            "the two config leaves must be optional, or removal would not be a valid fix"
        );
    }

    #[test]
    fn every_entry_changes_the_field_it_names() {
        for entry in LEGACY_DEFAULTS {
            assert_ne!(
                entry.legacy_value, entry.supported_value,
                "{} would be a no-op",
                entry.field
            );
            assert!(
                entry.merge_patch.contains(entry.key),
                "the patch for {} must name the field it sets",
                entry.field
            );
            assert!(
                entry.merge_patch.contains(entry.supported_value),
                "the patch for {} must set the supported value",
                entry.field
            );
            assert!(
                entry.field.ends_with(entry.key),
                "{} and {} describe different fields",
                entry.field,
                entry.key
            );
            if let Some(parent) = entry.parent {
                assert_eq!(
                    entry.field,
                    format!("{parent}.{}", entry.key),
                    "{} must sit directly inside the block it names",
                    entry.field
                );
            }
        }
    }

    /// A patch that nulls the *block* would delete every sibling setting in it.
    /// Removals must reach the leaf and nothing else.
    #[test]
    fn removals_null_the_leaf_and_never_the_block() {
        for entry in LEGACY_DEFAULTS.iter().filter(|e| e.removes_the_key()) {
            let parent = entry.parent.unwrap_or_else(|| {
                panic!("{} removes a key, so it must name its block", entry.field)
            });
            let block = parent
                .rsplit('.')
                .next()
                .expect("a parent path always has a last segment");

            assert!(
                entry
                    .merge_patch
                    .contains(&format!(r#""{}":null"#, entry.key)),
                "{} must be nulled by name: {}",
                entry.field,
                entry.merge_patch
            );
            assert!(
                !entry.merge_patch.contains(&format!(r#""{block}":null"#)),
                "{} must not null the whole {parent} block: {}",
                entry.field,
                entry.merge_patch
            );
            assert!(
                entry.merge_patch.contains(&format!(r#""{block}":{{"#)),
                "{} must patch inside {parent}, so siblings survive: {}",
                entry.field,
                entry.merge_patch
            );
            assert!(
                entry.needs_corrected_crds_first(),
                "a removal is undone by v0.3.0 defaulting, so {} must say so",
                entry.field
            );
        }

        assert!(
            LEGACY_DEFAULTS.iter().any(LegacyDefault::removes_the_key),
            "the two spec.config leaves accept no value, so removal must be tabled"
        );
        assert!(
            LEGACY_DEFAULTS
                .iter()
                .any(|e| !e.needs_corrected_crds_first()),
            "value assignments need no CRD ordering; the flag must not be universal"
        );
    }

    #[test]
    fn patch_commands_are_namespace_aware() {
        for entry in LEGACY_DEFAULTS {
            let command = entry.patch_command("my-resource", DOCS_NAMESPACE);
            assert!(
                command.contains(&format!("-n {DOCS_NAMESPACE}")),
                "a patch without a namespace targets whatever kubectl defaults to: {command}"
            );
            assert!(command.contains("--type merge"), "{command}");
            assert!(command.contains(entry.resource), "{command}");
        }
    }

    #[test]
    fn remediation_claims_v0_3_0_provenance_only_for_the_legacy_value() {
        let replicas = legacy_default("spec.replicas").expect("spec.replicas is tabled");

        let legacy = replicas.remediation(true);
        assert!(legacy.contains("v0.3.0"), "{legacy}");
        assert!(legacy.contains(UPGRADE_GUIDE), "{legacy}");
        assert!(legacy.contains("spec.replicas: 1"), "{legacy}");
        assert!(legacy.contains(r#"{"spec":{"replicas":1}}"#), "{legacy}");

        // A hand-written `replicas: 5` did not come from an upgrade, so the
        // message must not blame one — but it still has to say what to do.
        let deliberate = replicas.remediation(false);
        assert!(!deliberate.contains("v0.3.0"), "{deliberate}");
        assert!(deliberate.contains(UPGRADE_GUIDE), "{deliberate}");
        assert!(deliberate.contains("spec.replicas: 1"), "{deliberate}");
    }

    /// A nested default was only injected where the block existed. Claiming it
    /// landed in every resource would send readers hunting through topics that
    /// never had the field.
    #[test]
    fn nested_provenance_names_the_block_that_had_to_exist() {
        let min_isr =
            legacy_default("spec.config.minInsyncReplicas").expect("minInsyncReplicas is tabled");
        let message = min_isr.remediation(true);

        assert!(message.contains("spec.config"), "{message}");
        assert!(message.contains("only"), "{message}");
        assert!(
            !message.contains("created against those CRDs"),
            "the top-level wording overclaims for a nested default: {message}"
        );
        assert!(message.contains("does not rewrite specs"), "{message}");
    }

    /// Removals have to say both what to write and when it takes effect: a
    /// removal applied under the v0.3.0 CRDs is silently undone.
    #[test]
    fn a_removal_explains_the_null_and_the_crd_ordering() {
        for entry in LEGACY_DEFAULTS.iter().filter(|e| e.removes_the_key()) {
            let message = entry.remediation(true);

            assert!(
                message.contains(&entry.supported_yaml()),
                "the reader must be told what to write: {message}"
            );
            assert!(
                message.contains("Remove the key"),
                "`null` alone does not read as a deletion: {message}"
            );
            assert!(
                message.contains("leaves every other"),
                "the reader must know siblings survive: {message}"
            );
            assert!(
                message.contains("corrected CRDs are installed"),
                "a removal under the old CRDs is undone; say so: {message}"
            );
            assert!(message.contains(entry.merge_patch), "{message}");
        }
    }

    #[test]
    fn value_lookup_decides_provenance_from_the_observed_value() {
        assert!(
            remediation_for_value("spec.retention.retentionMs", "604800000").contains("v0.3.0")
        );
        assert!(!remediation_for_value("spec.retention.retentionMs", "3600000").contains("v0.3.0"));
        assert!(
            remediation_for_value("spec.retention.retentionMs", "3600000")
                .contains("spec.retention.retentionMs: -1"),
            "a deliberate value still needs the fix"
        );
        assert!(
            remediation_for_value("spec.config.segmentBytes", "1024").is_empty(),
            "a field that was never a v0.3.0 default must add nothing"
        );
    }
}
