//! Hermetic validation of the YAML examples in the documentation against the
//! generated CRD schemas.
//!
//! The README and crate docs are the first thing anyone applies, and they had
//! drifted from the schemas they claimed to document: a topic example used
//! `retention: {ms, bytes}` where the CRD has `retentionMs`/`retentionBytes`,
//! `compression: lz4` where the CRD has `compression: {type: ...}`, and a user
//! example used an `acls:` field that does not exist. Every one of those
//! examples is rejected by the API server, so the documented quick start could
//! not work — and nothing failed until a user tried it.
//!
//! These tests extract every fenced `yaml` block from the docs and check it
//! against the schema the operator actually generates: unknown properties,
//! missing required properties, and wrong types all fail here. They also assert
//! the docs do not advertise resources the operator does not install, or the
//! stale operator image the deployment placeholder exists to prevent.
//!
//! Nothing here needs a cluster, a network, or Docker.

// unwrap/expect are acceptable in tests; the crate-wide lint targets production code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use streamline_operator::crd::generate::{generated_crds, GeneratedCrd, Reconciliation};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(relative: &str) -> String {
    let path = repo_root().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {relative}: {e}"))
}

/// Documentation files whose YAML examples must be applicable as written.
///
/// `docs/UPGRADING.md` is included so the post-upgrade manifests it prints are
/// held to the same standard as the quick start: an upgrade guide whose "after"
/// example the operator rejects would be worse than no guide. The v0.3.0
/// shapes it shows are deliberately in `text` blocks — they are output to
/// recognise, not YAML to apply.
const DOCUMENTED_SOURCES: &[&str] = &[
    "README.md",
    "docs/API.md",
    "docs/UPGRADING.md",
    "src/lib.rs",
];

/// A fenced YAML example lifted out of a documentation file.
#[derive(Debug)]
struct Example {
    source: &'static str,
    /// 1-based line of the opening fence, for actionable failures.
    line: usize,
    body: String,
}

/// Extract fenced ```yaml blocks.
///
/// `src/lib.rs` carries its examples inside `//!` doc comments, so the prefix
/// is stripped before parsing — otherwise the crate docs would be exempt from
/// exactly the check they need.
fn extract_yaml_examples(source: &'static str) -> Vec<Example> {
    let raw = read(source);
    let is_rust_doc = source.ends_with(".rs");

    let mut examples = Vec::new();
    let mut current: Option<(usize, Vec<String>)> = None;

    for (index, raw_line) in raw.lines().enumerate() {
        let line = if is_rust_doc {
            match raw_line.trim_start().strip_prefix("//!") {
                Some(rest) => rest.strip_prefix(' ').unwrap_or(rest),
                // A non-doc line ends any block we were in; doc comments are
                // contiguous.
                None => {
                    current = None;
                    continue;
                }
            }
        } else {
            raw_line
        };

        let fence = line.trim_start();
        match &mut current {
            Some((start, collected)) => {
                if fence.starts_with("```") {
                    examples.push(Example {
                        source,
                        line: *start,
                        body: collected.join("\n"),
                    });
                    current = None;
                } else {
                    collected.push(line.to_string());
                }
            }
            None => {
                if fence == "```yaml" || fence == "```yml" {
                    current = Some((index + 1, Vec::new()));
                }
            }
        }
    }

    examples
}

/// Every Streamline custom resource example across the documented sources.
fn streamline_examples() -> Vec<(Example, serde_yaml::Mapping)> {
    let mut found = Vec::new();

    for source in DOCUMENTED_SOURCES {
        for example in extract_yaml_examples(source) {
            let parsed: serde_yaml::Value =
                serde_yaml::from_str(&example.body).unwrap_or_else(|e| {
                    panic!(
                        "{}:{} is not valid YAML: {e}\n{}",
                        example.source, example.line, example.body
                    )
                });

            let Some(mapping) = parsed.as_mapping() else {
                continue;
            };
            let api_version = mapping
                .get(serde_yaml::Value::from("apiVersion"))
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if !api_version.starts_with("streamline.io/") {
                continue;
            }
            found.push((example, mapping.clone()));
        }
    }

    assert!(
        !found.is_empty(),
        "expected the docs to contain Streamline custom resource examples"
    );
    found
}

fn crd_for(kind: &str) -> Option<GeneratedCrd> {
    generated_crds()
        .unwrap()
        .into_iter()
        .find(|c| c.kind == kind)
}

/// The `openAPIV3Schema` of a generated CRD's single served version.
fn schema_for(crd: &GeneratedCrd) -> serde_json::Value {
    let parsed: serde_json::Value = serde_yaml::from_str(&crd.yaml)
        .unwrap_or_else(|e| panic!("{} does not parse: {e}", crd.kind));
    parsed["spec"]["versions"][0]["schema"]["openAPIV3Schema"].clone()
}

/// Recursively check a documented value against an OpenAPI v3 schema node.
///
/// Deliberately strict about *unknown* properties: the whole point is to catch
/// documented fields the CRD does not have, which the API server would reject
/// (`x-kubernetes-preserve-unknown-fields` is not set on these schemas) but
/// serde would silently ignore.
fn check_against_schema(
    value: &serde_json::Value,
    schema: &serde_json::Value,
    path: &str,
    errors: &mut Vec<String>,
) {
    // `additionalProperties: {...}` describes a free-form map (e.g. labels).
    if let Some(additional) = schema.get("additionalProperties") {
        if additional.is_object() {
            if let Some(map) = value.as_object() {
                for (key, entry) in map {
                    check_against_schema(entry, additional, &format!("{path}.{key}"), errors);
                }
            }
            return;
        }
    }

    match schema.get("type").and_then(|t| t.as_str()) {
        Some("object") => {
            let Some(map) = value.as_object() else {
                errors.push(format!("{path} must be an object"));
                return;
            };

            let properties = schema.get("properties").and_then(|p| p.as_object());
            if let Some(properties) = properties {
                for key in map.keys() {
                    if !properties.contains_key(key) {
                        let mut known: Vec<&str> = properties.keys().map(String::as_str).collect();
                        known.sort_unstable();
                        errors.push(format!(
                            "{path}.{key} is not a field of the generated schema (known: {})",
                            known.join(", ")
                        ));
                    }
                }
                for (key, entry) in map {
                    if let Some(property) = properties.get(key) {
                        check_against_schema(entry, property, &format!("{path}.{key}"), errors);
                    }
                }
            }

            if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
                for field in required.iter().filter_map(|f| f.as_str()) {
                    if !map.contains_key(field) {
                        errors.push(format!(
                            "{path}.{field} is required but the example omits it"
                        ));
                    }
                }
            }
        }
        Some("array") => {
            let Some(items) = value.as_array() else {
                errors.push(format!("{path} must be an array"));
                return;
            };
            if let Some(item_schema) = schema.get("items") {
                for (index, item) in items.iter().enumerate() {
                    check_against_schema(item, item_schema, &format!("{path}[{index}]"), errors);
                }
            }
        }
        Some("string") => {
            if !value.is_string() {
                errors.push(format!("{path} must be a string, got {value}"));
            } else if let Some(allowed) = schema.get("enum").and_then(|e| e.as_array()) {
                if !allowed.contains(value) {
                    errors.push(format!(
                        "{path} = {value} is not one of the schema's accepted values"
                    ));
                }
            }
        }
        Some("integer") => {
            if !value.is_i64() && !value.is_u64() {
                errors.push(format!("{path} must be an integer, got {value}"));
            }
        }
        Some("number") => {
            if !value.is_number() {
                errors.push(format!("{path} must be a number, got {value}"));
            }
        }
        Some("boolean") if !value.is_boolean() => {
            errors.push(format!("{path} must be a boolean, got {value}"));
        }
        _ => {}
    }
}

fn assert_spec_matches_generated_schema(kind: &str, spec: &serde_json::Value, label: &str) {
    let crd = crd_for(kind).unwrap_or_else(|| panic!("missing generated CRD for {kind}"));
    let schema = schema_for(&crd);
    let mut errors = Vec::new();
    check_against_schema(spec, &schema["properties"]["spec"], "spec", &mut errors);
    assert!(
        errors.is_empty(),
        "{label} does not match the generated {kind} schema:\n  {}",
        errors.join("\n  ")
    );
}

// ---------------------------------------------------------------------------
// Examples match the generated schemas
// ---------------------------------------------------------------------------

#[test]
fn documented_examples_reference_kinds_this_operator_defines() {
    for (example, mapping) in streamline_examples() {
        let kind = mapping
            .get(serde_yaml::Value::from("kind"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        assert!(
            crd_for(&kind).is_some(),
            "{}:{} documents kind `{kind}`, which no CRD in this crate defines",
            example.source,
            example.line
        );

        let api_version = mapping
            .get(serde_yaml::Value::from("apiVersion"))
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert_eq!(
            api_version, "streamline.io/v1alpha1",
            "{}:{} documents an apiVersion the CRDs do not serve",
            example.source, example.line
        );
    }
}

#[test]
fn documented_examples_validate_against_the_generated_schemas() {
    let mut failures: Vec<String> = Vec::new();

    for (example, mapping) in streamline_examples() {
        let kind = mapping
            .get(serde_yaml::Value::from("kind"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let Some(crd) = crd_for(&kind) else {
            continue;
        };
        let Some(spec) = mapping.get(serde_yaml::Value::from("spec")) else {
            continue;
        };

        let schema = schema_for(&crd);
        let spec_schema = &schema["properties"]["spec"];
        let spec_json: serde_json::Value =
            serde_json::to_value(spec).expect("spec must convert to JSON");

        let mut errors = Vec::new();
        check_against_schema(&spec_json, spec_schema, "spec", &mut errors);

        for error in errors {
            failures.push(format!(
                "{}:{} ({kind}) {error}",
                example.source, example.line
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "documented examples do not match the generated CRD schemas:\n  {}",
        failures.join("\n  ")
    );
}

/// Schema validation alone would not catch a value that parses as the right
/// JSON type but that the controller rejects, so the supported-behaviour
/// invariants are asserted directly on the documented specs.
#[test]
fn documented_examples_describe_behaviour_the_operator_supports() {
    for (example, mapping) in streamline_examples() {
        let kind = mapping
            .get(serde_yaml::Value::from("kind"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let Some(spec) = mapping.get(serde_yaml::Value::from("spec")) else {
            continue;
        };
        let where_ = format!("{}:{}", example.source, example.line);

        match kind.as_str() {
            "StreamlineCluster" => {
                let cluster: streamline_operator::ClusterSpec =
                    serde_yaml::from_value(spec.clone())
                        .unwrap_or_else(|e| panic!("{where_} is not a valid ClusterSpec: {e}"));

                assert!(
                    cluster.validate().is_ok(),
                    "{where_} documents a cluster the operator rejects: {:?}",
                    cluster.validate().unwrap_err()
                );
                assert_eq!(
                    cluster.replicas, 1,
                    "{where_} documents replicas={} but the operator renders standalone brokers",
                    cluster.replicas
                );
                assert!(
                    cluster
                        .autoscaling
                        .as_ref()
                        .is_none_or(|autoscaling| !autoscaling.enabled),
                    "{where_} documents autoscaling, which the operator rejects"
                );

                // Scheduling settings the pod template never carries. These
                // were advertised in the schema and in the quick start, and
                // then rendered nowhere: a cluster that asked to be spread
                // across nodes was reported Ready while sitting wherever the
                // scheduler happened to put it. `nodeSelector` is deliberately
                // absent from this list — it *is* rendered.
                assert!(
                    !cluster.pod_anti_affinity,
                    "{where_} documents podAntiAffinity, which the operator renders \
                     nowhere and now rejects"
                );
                assert!(
                    cluster
                        .rack_awareness
                        .as_ref()
                        .is_none_or(|rack| !rack.enabled),
                    "{where_} documents rackAwareness, which the operator renders \
                     nowhere and now rejects"
                );
                assert!(
                    cluster.tolerations.is_empty(),
                    "{where_} documents {} toleration(s), which the operator renders \
                     nowhere and now rejects",
                    cluster.tolerations.len()
                );

                // Every documented env entry must map exactly onto a container
                // variable; anything else would reach the broker as an empty
                // string.
                for (index, env) in cluster.env.iter().enumerate() {
                    assert!(
                        env.resolve_source().is_ok(),
                        "{where_} documents env[{index}] the operator cannot render: {}",
                        env.resolve_source().unwrap_err()
                    );
                }
            }
            "StreamlineTopic" => {
                let topic: streamline_operator::TopicSpec = serde_yaml::from_value(spec.clone())
                    .unwrap_or_else(|e| panic!("{where_} is not a valid TopicSpec: {e}"));

                assert_eq!(
                    topic.replication_factor, 1,
                    "{where_} documents replicationFactor={}, which the operator rejects",
                    topic.replication_factor
                );

                // The documented topic must survive the controller's own
                // fail-closed gate, which rejects every setting the Streamline
                // server does not apply.
                let defaults = streamline_operator::TopicSpec::config_defaults();
                assert_eq!(
                    topic.retention.retention_ms, defaults.retention_ms,
                    "{where_} documents a retention the server never applies"
                );
                assert_eq!(
                    topic.retention.retention_bytes, defaults.retention_bytes,
                    "{where_} documents a retention size the server never applies"
                );
                assert_eq!(
                    topic.retention.cleanup_policy, defaults.cleanup_policy,
                    "{where_} documents a cleanup policy the server never applies"
                );
                assert_eq!(
                    topic.compression.r#type, defaults.compression_type,
                    "{where_} documents a compression type the server never applies"
                );
                assert!(
                    topic.config.min_insync_replicas.is_none()
                        && topic.config.max_message_bytes.is_none()
                        && topic.config.segment_bytes.is_none()
                        && topic.config.index_interval_bytes.is_none()
                        && topic.config.flush_interval_ms.is_none()
                        && topic.config.flush_messages.is_none()
                        && topic.config.custom.is_empty(),
                    "{where_} documents topic config overrides the server discards"
                );
            }
            "StreamlineUser" => {
                let _user: streamline_operator::UserSpec = serde_yaml::from_value(spec.clone())
                    .unwrap_or_else(|e| panic!("{where_} is not a valid UserSpec: {e}"));
            }
            _ => {}
        }
    }
}

/// The cloud control plane's checked-in contract fixture emits these exact
/// spec shapes. Keep this test local and literal so a standalone operator
/// checkout remains hermetic while changes to defaults still have to satisfy
/// both the generated OpenAPI schema and the controller's fail-closed rules.
///
/// Source fixture (read-only during coordinated development):
/// `streamline-cloud/control-plane/tests/fixtures/operator-contract-crs.json`.
#[test]
fn cloud_generated_specs_match_schema_and_runtime_validation() {
    let cluster_without_affinity = serde_json::json!({
        "httpPort": 9094,
        "image": "ghcr.io/streamlinelabs/streamline-cloud-data-plane@sha256:1111111111111111111111111111111111111111111111111111111111111111",
        "imagePullPolicy": "IfNotPresent",
        "kafkaPort": 9092,
        "logLevel": "info",
        "metricsEnabled": true,
        "replicas": 1,
        "resources": {
            "limits": {"cpu": "8", "memory": "16Gi"},
            "requests": {"cpu": "2", "memory": "4Gi"}
        },
        "storage": {"size": "10Gi", "storageClassName": "gp3"},
        "tls": {"enabled": true, "secretName": "orders-tls"},
        "updateStrategy": {"maxUnavailable": 1, "type": "RollingUpdate"}
    });
    assert_spec_matches_generated_schema(
        "StreamlineCluster",
        &cluster_without_affinity,
        "cloud cluster with omitted podAntiAffinity",
    );
    let omitted: streamline_operator::ClusterSpec =
        serde_json::from_value(cluster_without_affinity.clone()).unwrap();
    assert!(!omitted.pod_anti_affinity);
    assert!(omitted.validate().is_ok(), "{:?}", omitted.validate());

    let mut cluster_with_false = cluster_without_affinity;
    cluster_with_false["podAntiAffinity"] = serde_json::Value::Bool(false);
    assert_spec_matches_generated_schema(
        "StreamlineCluster",
        &cluster_with_false,
        "cloud cluster with podAntiAffinity=false",
    );
    let explicit_false: streamline_operator::ClusterSpec =
        serde_json::from_value(cluster_with_false).unwrap();
    assert!(!explicit_false.pod_anti_affinity);
    assert!(
        explicit_false.validate().is_ok(),
        "{:?}",
        explicit_false.validate()
    );
    let cluster_schema = schema_for(&crd_for("StreamlineCluster").unwrap());
    assert_eq!(
        cluster_schema["properties"]["spec"]["properties"]["podAntiAffinity"]["default"],
        serde_json::Value::Bool(false),
        "the API server must default an omitted podAntiAffinity to the supported value"
    );

    let topic_without_retention = serde_json::json!({
        "clusterRef": "orders",
        "partitions": 6,
        "replicationFactor": 1
    });
    assert_spec_matches_generated_schema(
        "StreamlineTopic",
        &topic_without_retention,
        "cloud topic with omitted retention",
    );
    let omitted: streamline_operator::TopicSpec =
        serde_json::from_value(topic_without_retention.clone()).unwrap();
    assert_eq!(omitted.retention.retention_ms, -1);
    assert!(streamline_operator::TopicController::unsupported_fields(&omitted).is_empty());

    let mut topic_with_unlimited_retention = topic_without_retention.clone();
    topic_with_unlimited_retention["retention"] = serde_json::json!({
        "retentionMs": -1,
        "retentionBytes": -1
    });
    assert_spec_matches_generated_schema(
        "StreamlineTopic",
        &topic_with_unlimited_retention,
        "cloud topic with retentionMs=-1",
    );
    let explicit_unlimited: streamline_operator::TopicSpec =
        serde_json::from_value(topic_with_unlimited_retention).unwrap();
    assert_eq!(explicit_unlimited.retention.retention_ms, -1);
    assert!(
        streamline_operator::TopicController::unsupported_fields(&explicit_unlimited).is_empty()
    );
    let topic_schema = schema_for(&crd_for("StreamlineTopic").unwrap());
    assert_eq!(
        topic_schema["properties"]["spec"]["properties"]["retention"]["default"]["retentionMs"],
        serde_json::json!(-1),
        "the API server must default omitted retention to unlimited"
    );
    assert_eq!(
        topic_schema["properties"]["spec"]["properties"]["retention"]["properties"]["retentionMs"]
            ["default"],
        serde_json::json!(-1),
        "an explicitly present retention block must default retentionMs to unlimited"
    );

    // The schema accepts an integer here, but the controller must fail closed
    // when the cloud asks for a retention policy the server cannot apply.
    let mut unsupported_retention = topic_without_retention;
    unsupported_retention["retention"] = serde_json::json!({"retentionMs": 3_600_000});
    assert_spec_matches_generated_schema(
        "StreamlineTopic",
        &unsupported_retention,
        "cloud topic with a non-default retention",
    );
    let unsupported: streamline_operator::TopicSpec =
        serde_json::from_value(unsupported_retention).unwrap();
    assert!(
        streamline_operator::TopicController::unsupported_fields(&unsupported)
            .iter()
            .any(|message| message.contains("retention.retentionMs"))
    );
}

// ---------------------------------------------------------------------------
// Docs do not advertise what the operator does not install
// ---------------------------------------------------------------------------

/// Every documented example of a schema-only kind must be labelled as such
/// within its section, so a reader cannot mistake it for something they can
/// `kubectl apply`.
#[test]
fn schema_only_kinds_are_documented_as_not_installed() {
    let unsupported: Vec<&'static str> = generated_crds()
        .unwrap()
        .iter()
        .filter(|c| matches!(c.reconciliation, Reconciliation::None(_)))
        .map(|c| c.kind)
        .collect();

    for source in DOCUMENTED_SOURCES {
        let body = read(source);
        for kind in &unsupported {
            let Some(position) = body.find(kind) else {
                continue;
            };

            // Character-based windows: the docs contain box-drawing characters,
            // so byte slicing would split a multi-byte character.
            let chars: Vec<char> = body.chars().collect();
            let prefix_chars = body[..position].chars().count();
            let start = prefix_chars.saturating_sub(600);
            let end = (prefix_chars + kind.chars().count() + 600).min(chars.len());
            let window: String = chars[start..end].iter().collect();

            let window = window.to_ascii_lowercase();
            assert!(
                window.contains("not installed")
                    || window.contains("schema-only")
                    || window.contains("schema only"),
                "{source} mentions {kind} without saying it is not installed"
            );
        }
    }
}

/// The docs must not tell users to install a CRD the kustomization excludes.
#[test]
fn docs_do_not_instruct_installing_unsupported_crds() {
    let kustomization = read("deploy/crds/kustomization.yaml");

    for crd in generated_crds().unwrap() {
        if crd.is_installed() {
            continue;
        }
        assert!(
            !kustomization.contains(&crd.file_name),
            "{} is listed for installation but nothing reconciles it",
            crd.kind
        );
        // Naming the generated manifest is fine (it is how you read the
        // schema); telling someone to *apply* it is not.
        for source in DOCUMENTED_SOURCES {
            for (index, line) in read(source).lines().enumerate() {
                if !line.contains(&crd.file_name) {
                    continue;
                }
                let lowered = line.to_ascii_lowercase();
                assert!(
                    !lowered.contains("apply") && !lowered.contains("create -f"),
                    "{source}:{} instructs installing {}, which nothing reconciles: {}",
                    index + 1,
                    crd.file_name,
                    line.trim()
                );
            }
        }
    }
}

/// Shipped manifests must not declare integrations for kinds the operator never
/// installs: an Argo CD health check (or action) for a schema-only CRD tells
/// users the operator manages something it does not.
#[test]
fn shipped_manifests_declare_no_integrations_for_schema_only_kinds() {
    let unsupported: Vec<&'static str> = generated_crds()
        .unwrap()
        .iter()
        .filter(|c| matches!(c.reconciliation, Reconciliation::None(_)))
        .map(|c| c.kind)
        .collect();

    let argocd = read("deploy/argocd/argocd-health-checks.yaml");
    for (index, line) in argocd.lines().enumerate() {
        if !line.trim_start().starts_with("resource.customizations.") {
            continue;
        }
        for kind in &unsupported {
            assert!(
                !line.contains(kind),
                "deploy/argocd/argocd-health-checks.yaml:{} customises {kind}, \
                 which is never installed: {}",
                index + 1,
                line.trim()
            );
        }
    }
}

/// Every `status.phase` an Argo CD health check tests for must be a phase the
/// CRD can actually report. Checks for phases that do not exist (the manifest
/// tested for "Provisioning", "Error", and "Creating") fall through to
/// "Unknown", so a genuinely failing resource looks merely indeterminate.
#[test]
fn argocd_health_checks_only_test_phases_the_crds_define() {
    let argocd = read("deploy/argocd/argocd-health-checks.yaml");

    // Collect the phase enums the generator emits, per kind.
    let mut known: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    for crd in generated_crds().unwrap() {
        let schema = schema_for(&crd);
        let phases = schema["properties"]["status"]["properties"]["phase"]["enum"]
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        known.insert(crd.kind, phases);
    }

    let mut current_kind: Option<&'static str> = None;
    for (index, line) in argocd.lines().enumerate() {
        if let Some(rest) = line
            .trim_start()
            .strip_prefix("resource.customizations.health.streamline.io_")
        {
            let kind = rest.trim_end_matches(": |");
            current_kind = known.keys().copied().find(|k| *k == kind);
            continue;
        }

        let Some(kind) = current_kind else { continue };
        // Comments outside a block reset the context.
        if line.trim_start().starts_with('#') {
            continue;
        }

        for chunk in line.split("obj.status.phase == \"").skip(1) {
            let Some(phase) = chunk.split('"').next() else {
                continue;
            };
            let phases = &known[kind];
            assert!(
                phases.iter().any(|p| p == phase),
                "deploy/argocd/argocd-health-checks.yaml:{} tests {kind} for phase \
                 `{phase}`, which the CRD never reports (known: {phases:?})",
                index + 1
            );
        }
    }

    assert!(
        current_kind.is_some(),
        "no Argo CD health customization was inspected"
    );
}

/// Neither the docs nor the manifests may advertise the released operator image
/// that predates this tree; `deploy/` ships an unpullable placeholder instead.
#[test]
fn nothing_advertises_a_released_operator_image() {
    let mut offenders: Vec<String> = Vec::new();

    let scanned: Vec<&str> = DOCUMENTED_SOURCES
        .iter()
        .copied()
        .chain([
            "deploy/operator.yaml",
            "deploy/kustomization.yaml",
            "docs/ENVIRONMENT.md",
            "CLAUDE.md",
        ])
        .collect();

    for source in scanned {
        for (index, line) in read(source).lines().enumerate() {
            // Prose and comments may name the old tag while explaining it.
            let code = match source.ends_with(".md") {
                true => line,
                false => line.split('#').next().unwrap_or_default(),
            };
            if code.contains("ghcr.io/streamlinelabs/streamline-operator:")
                && !code.contains("REPLACE_WITH_RELEASED_IMAGE")
            {
                offenders.push(format!("{source}:{}: {}", index + 1, line.trim()));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "a runnable operator image is advertised; use the placeholder or a \
         digest supplied at deploy time:\n  {}",
        offenders.join("\n  ")
    );
}

/// The broker image is a different image from the operator's, and it *is*
/// published — documenting it is correct, so this pins the distinction rather
/// than banning every image reference.
#[test]
fn the_documented_broker_image_is_the_server_not_the_operator() {
    let default_image = read("src/crd/cluster.rs")
        .lines()
        .skip_while(|l| !l.contains("fn default_image()"))
        .find_map(|l| l.trim().strip_prefix('"'))
        .map(|l| l.split('"').next().unwrap_or_default().to_string())
        .expect("default_image must return a literal");

    assert!(
        !default_image.contains("streamline-operator"),
        "brokers must not default to the operator image"
    );

    for (example, mapping) in streamline_examples() {
        let Some(image) = mapping
            .get(serde_yaml::Value::from("spec"))
            .and_then(|s| s.get(serde_yaml::Value::from("image")))
            .and_then(|v| v.as_str())
        else {
            continue;
        };
        assert!(
            !image.contains("streamline-operator"),
            "{}:{} documents the operator image as a broker image",
            example.source,
            example.line
        );
    }
}

// ---------------------------------------------------------------------------
// Documented namespaces match the namespace the operator actually watches
// ---------------------------------------------------------------------------

/// The single namespace the shipped `deploy/` watches, derived from the
/// manifests rather than hard-coded here.
///
/// `deploy/operator.yaml` passes `--namespace=$(OPERATOR_NAMESPACE)` and binds
/// that variable to the Pod's own namespace through the downward API, so the
/// watched namespace *is* the Deployment's namespace. Deriving it means moving
/// the operator to a different namespace re-points this expectation instead of
/// silently invalidating it.
fn shipped_watched_namespace() -> String {
    let deployment: serde_yaml::Value =
        serde_yaml::from_str(&read("deploy/operator.yaml")).expect("deploy/operator.yaml parses");

    let container = deployment["spec"]["template"]["spec"]["containers"]
        .as_sequence()
        .and_then(|containers| containers.first())
        .expect("the Deployment must define a container");

    let flag = container["args"]
        .as_sequence()
        .expect("the operator container must pass args")
        .iter()
        .filter_map(|arg| arg.as_str())
        .find_map(|arg| arg.strip_prefix("--namespace="))
        .expect("the shipped Deployment must pass --namespace")
        .to_string();

    // A literal value needs no resolution; `$(VAR)` is expanded by the kubelet
    // from the container's env.
    let Some(variable) = flag
        .strip_prefix("$(")
        .and_then(|rest| rest.strip_suffix(')'))
    else {
        return flag;
    };

    let entry = container["env"]
        .as_sequence()
        .expect("the operator container must define env")
        .iter()
        .find(|env| env["name"].as_str() == Some(variable))
        .unwrap_or_else(|| panic!("{flag} is not defined in the container env"));

    // Only the downward-API form resolves to the namespace the operator runs
    // in; a literal env value could point anywhere the Role does not cover.
    let field_path = entry["valueFrom"]["fieldRef"]["fieldPath"]
        .as_str()
        .unwrap_or_default();
    assert_eq!(
        field_path, "metadata.namespace",
        "{flag} must be bound to the Pod's own namespace"
    );

    deployment["metadata"]["namespace"]
        .as_str()
        .expect("the Deployment must declare the namespace it is installed into")
        .to_string()
}

/// Why a documented example's namespace is wrong, or `None` if it is right.
///
/// Factored out so the guard test below can prove the comparison still
/// rejects something.
fn namespace_violation(namespace: Option<&str>, watched: &str) -> Option<String> {
    match namespace {
        Some(ns) if ns == watched => None,
        Some(ns) => Some(format!(
            "is created in `{ns}`, but the shipped operator watches only `{watched}`, \
             so nothing would ever reconcile it"
        )),
        None => Some(format!(
            "omits metadata.namespace, so it lands in whichever namespace kubectl \
             defaults to (usually `default`) rather than the watched `{watched}`"
        )),
    }
}

/// Every documented custom resource must be created in the namespace the
/// shipped operator watches.
///
/// The quick start said `namespace: default` while `deploy/` ships a
/// `--namespace=$(OPERATOR_NAMESPACE)` watch and a namespaced `Role` in
/// `streamline-system`. Following the README therefore created a resource in a
/// namespace the operator neither watches nor may read: the API server accepts
/// it, and then nothing happens — no status, no events, no error. That is the
/// worst failure mode a first run can have, and no test caught it because the
/// examples were schema-valid.
#[test]
fn documented_examples_live_in_the_namespace_the_operator_watches() {
    let watched = shipped_watched_namespace();
    let mut failures: Vec<String> = Vec::new();

    for (example, mapping) in streamline_examples() {
        let namespace = mapping
            .get(serde_yaml::Value::from("metadata"))
            .and_then(|metadata| metadata.get(serde_yaml::Value::from("namespace")))
            .and_then(|value| value.as_str());

        if let Some(reason) = namespace_violation(namespace, &watched) {
            failures.push(format!("{}:{} {reason}", example.source, example.line));
        }
    }

    assert!(
        failures.is_empty(),
        "documented examples target a namespace the operator does not watch:\n  {}",
        failures.join("\n  ")
    );
}

/// The watch is only useful where the RBAC authorises it: the namespace the
/// examples use, the namespace the Deployment runs in, and the namespace the
/// `Role`/`RoleBinding` cover must all be the same one.
#[test]
fn the_watched_namespace_is_the_one_the_shipped_rbac_authorises() {
    let watched = shipped_watched_namespace();
    assert!(
        !watched.trim().is_empty(),
        "an empty watch is cluster-wide mode, which the shipped namespaced Role \
         cannot authorise"
    );

    for manifest in [
        "deploy/rbac/role.yaml",
        "deploy/rbac/role-binding.yaml",
        "deploy/rbac/service-account.yaml",
    ] {
        let parsed: serde_yaml::Value =
            serde_yaml::from_str(&read(manifest)).unwrap_or_else(|e| panic!("{manifest}: {e}"));
        assert_eq!(
            parsed["metadata"]["namespace"].as_str(),
            Some(watched.as_str()),
            "{manifest} does not live in the watched namespace `{watched}`"
        );
    }

    let namespace: serde_yaml::Value =
        serde_yaml::from_str(&read("deploy/namespace.yaml")).expect("deploy/namespace.yaml parses");
    assert_eq!(
        namespace["metadata"]["name"].as_str(),
        Some(watched.as_str()),
        "deploy/namespace.yaml must create the namespace the operator watches"
    );
}

/// A guard on the guard: the namespace comparison must still reject the
/// namespace the quick start used to use, and a missing one.
#[test]
fn the_namespace_check_rejects_the_namespace_the_quick_start_used_to_use() {
    let watched = shipped_watched_namespace();

    assert!(
        namespace_violation(Some("default"), &watched).is_some(),
        "`default` must be rejected: it is not the namespace `deploy/` watches"
    );
    assert!(
        namespace_violation(None, &watched).is_some(),
        "an example without metadata.namespace must be rejected"
    );
    assert!(
        namespace_violation(Some(&watched), &watched).is_none(),
        "the watched namespace itself must be accepted"
    );
}

// ---------------------------------------------------------------------------
// The extractor itself
// ---------------------------------------------------------------------------

/// If the extractor silently found nothing, every test above would pass
/// vacuously.
#[test]
fn the_extractor_finds_examples_in_every_documented_source() {
    let mut per_source: BTreeMap<&str, usize> = BTreeMap::new();
    for source in DOCUMENTED_SOURCES {
        per_source.insert(source, extract_yaml_examples(source).len());
    }

    for (source, count) in &per_source {
        assert!(
            *count > 0,
            "{source} yielded no YAML examples; the extractor is broken or the \
             docs lost their examples"
        );
    }

    let custom_resources = streamline_examples().len();
    assert!(
        custom_resources >= 4,
        "expected several custom resource examples, found {custom_resources}"
    );
}

/// A guard on the guard: a deliberately wrong example must be rejected, so a
/// future refactor cannot quietly turn the schema check into a no-op.
#[test]
fn the_schema_check_rejects_a_field_the_crd_does_not_have() {
    let crd = crd_for("StreamlineTopic").expect("StreamlineTopic CRD");
    let schema = schema_for(&crd);

    // The pre-fix README used exactly this shape.
    let documented: serde_json::Value = serde_json::json!({
        "clusterRef": "my-cluster",
        "partitions": 6,
        "retention": { "ms": 604_800_000, "bytes": -1 },
    });

    let mut errors = Vec::new();
    check_against_schema(
        &documented,
        &schema["properties"]["spec"],
        "spec",
        &mut errors,
    );

    assert!(
        errors.iter().any(|e| e.contains("spec.retention.ms")),
        "the schema check must reject `retention.ms`, got {errors:?}"
    );
}

#[test]
fn the_schema_check_rejects_a_wrongly_typed_field() {
    let crd = crd_for("StreamlineTopic").expect("StreamlineTopic CRD");
    let schema = schema_for(&crd);

    // `compression` is an object in the schema, not the bare string the README
    // used to document.
    let documented: serde_json::Value = serde_json::json!({
        "clusterRef": "my-cluster",
        "compression": "lz4",
    });

    let mut errors = Vec::new();
    check_against_schema(
        &documented,
        &schema["properties"]["spec"],
        "spec",
        &mut errors,
    );

    assert!(
        errors.iter().any(|e| e.contains("spec.compression")),
        "the schema check must reject a string `compression`, got {errors:?}"
    );
}

#[test]
fn the_schema_check_rejects_a_missing_required_field() {
    let crd = crd_for("StreamlineTopic").expect("StreamlineTopic CRD");
    let schema = schema_for(&crd);

    let documented: serde_json::Value = serde_json::json!({ "partitions": 3 });

    let mut errors = Vec::new();
    check_against_schema(
        &documented,
        &schema["properties"]["spec"],
        "spec",
        &mut errors,
    );

    assert!(
        errors.iter().any(|e| e.contains("clusterRef")),
        "the schema check must require clusterRef, got {errors:?}"
    );
}
