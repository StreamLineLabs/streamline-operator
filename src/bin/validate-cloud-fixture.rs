//! Validate Streamline Cloud's generated custom resources with operator code.
//!
//! The organization contract workflow passes the checked-in Cloud fixture to
//! this binary. Deserialization therefore uses the real generated CR types,
//! and semantic acceptance uses the same validation functions as reconciliation
//! instead of a separately maintained reimplementation.

use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use streamline_operator::{StreamlineCluster, StreamlineTopic, TopicController};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CloudOperatorFixture {
    streamline_clusters: BTreeMap<String, StreamlineCluster>,
    streamline_topics: BTreeMap<String, StreamlineTopic>,
}

fn validate_fixture_contents(contents: &str, source: &str) -> Result<(usize, usize)> {
    let fixture: CloudOperatorFixture = serde_json::from_str(contents)
        .with_context(|| format!("failed to deserialize cloud fixture {source}"))?;

    if fixture.streamline_clusters.is_empty() {
        bail!("cloud fixture contains no StreamlineCluster resources");
    }
    if fixture.streamline_topics.is_empty() {
        bail!("cloud fixture contains no StreamlineTopic resources");
    }

    for (plan, cluster) in &fixture.streamline_clusters {
        if let Err(errors) = cluster.spec.validate() {
            bail!(
                "streamlineClusters.{plan} is rejected by ClusterSpec::validate(): {}",
                errors.join("; ")
            );
        }
    }

    for (plan, topic) in &fixture.streamline_topics {
        let unsupported = TopicController::unsupported_fields(&topic.spec);
        if !unsupported.is_empty() {
            bail!(
                "streamlineTopics.{plan} is rejected by \
                 TopicController::unsupported_fields(): {}",
                unsupported.join("; ")
            );
        }
    }

    Ok((
        fixture.streamline_clusters.len(),
        fixture.streamline_topics.len(),
    ))
}

fn validate_fixture(path: &Path) -> Result<(usize, usize)> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("failed to read cloud fixture {}", path.display()))?;
    validate_fixture_contents(&contents, &path.display().to_string())
}

fn fixture_argument() -> Result<OsString> {
    let mut args = env::args_os();
    let program = args
        .next()
        .unwrap_or_else(|| OsString::from("validate-cloud-fixture"));
    let Some(path) = args.next() else {
        bail!("usage: {} <cloud-fixture.json>", program.to_string_lossy());
    };
    if args.next().is_some() {
        bail!("usage: {} <cloud-fixture.json>", program.to_string_lossy());
    }
    Ok(path)
}

fn main() -> Result<()> {
    let path = fixture_argument()?;
    let path = Path::new(&path);
    let (clusters, topics) = validate_fixture(path)?;
    println!(
        "Validated {clusters} StreamlineCluster and {topics} StreamlineTopic \
         resource(s) from {} with operator code.",
        path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use serde_json::{json, Value};

    use super::validate_fixture_contents;

    fn valid_fixture() -> Value {
        json!({
            "streamlineClusters": {
                "pro": {
                    "apiVersion": "streamline.io/v1alpha1",
                    "kind": "StreamlineCluster",
                    "metadata": {"name": "orders"},
                    "spec": {
                        "replicas": 1,
                        "image": "registry.example/streamline-cloud-data-plane:test",
                        "imagePullPolicy": "IfNotPresent",
                        "resources": {
                            "requests": {"cpu": "500m", "memory": "1Gi"},
                            "limits": {"cpu": "2", "memory": "4Gi"}
                        },
                        "storage": {"size": "10Gi", "storageClassName": "gp3"},
                        "tls": {"enabled": true, "secretName": "orders-tls"},
                        "kafkaPort": 9092,
                        "httpPort": 9094,
                        "metricsEnabled": true,
                        "logLevel": "info",
                        "updateStrategy": {
                            "type": "RollingUpdate",
                            "maxUnavailable": 1
                        }
                    }
                }
            },
            "streamlineTopics": {
                "pro": {
                    "apiVersion": "streamline.io/v1alpha1",
                    "kind": "StreamlineTopic",
                    "metadata": {"name": "events", "namespace": "sl-fixture"},
                    "spec": {
                        "clusterRef": "orders",
                        "partitions": 3,
                        "replicationFactor": 1
                    }
                }
            }
        })
    }

    #[test]
    fn accepts_resources_the_operator_accepts() {
        let fixture = valid_fixture().to_string();
        assert_eq!(
            validate_fixture_contents(&fixture, "test fixture").unwrap(),
            (1, 1)
        );
    }

    #[test]
    fn rejects_a_cluster_the_operator_rejects() {
        let mut fixture = valid_fixture();
        fixture["streamlineClusters"]["pro"]["spec"]["podAntiAffinity"] = json!(true);

        let error = validate_fixture_contents(&fixture.to_string(), "test fixture")
            .expect_err("pod anti-affinity must be rejected")
            .to_string();
        assert!(error.contains("ClusterSpec::validate()"));
        assert!(error.contains("podAntiAffinity"));
    }

    #[test]
    fn rejects_a_topic_the_operator_rejects() {
        let mut fixture = valid_fixture();
        fixture["streamlineTopics"]["pro"]["spec"]["retention"] =
            json!({"retentionMs": 604_800_000});

        let error = validate_fixture_contents(&fixture.to_string(), "test fixture")
            .expect_err("finite retention must be rejected")
            .to_string();
        assert!(error.contains("TopicController::unsupported_fields()"));
        assert!(error.contains("retention.retentionMs"));
    }
}
