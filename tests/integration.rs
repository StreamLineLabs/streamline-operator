//! Explicitly gated integration tests for the Streamline Operator.
//!
//! `cargo test` is hermetic by design: nothing in the default test run contacts a
//! Kubernetes API server or a live Streamline broker. The tests that *do* need
//! those services are marked `#[ignore]` and only run when asked for explicitly:
//!
//! ```bash
//! make integration-up        # docker compose -f docker-compose.test.yml up -d --wait
//! make test-integration      # cargo test --test integration -- --ignored
//! make integration-down
//! ```
//!
//! Every networked assertion is bounded by [`IntegrationConfig::timeout`], so a
//! missing or wedged backend fails fast instead of hanging CI.
//!
//! ## Configuration
//!
//! | Variable | Description | Default |
//! |---|---|---|
//! | `STREAMLINE_TEST_IMAGE` | Server image used by `docker-compose.test.yml` | `ghcr.io/streamlinelabs/streamline:0.2.0` |
//! | `STREAMLINE_TEST_HTTP_PORT` | Host port mapped to the HTTP API | `9094` |
//! | `STREAMLINE_TEST_KAFKA_PORT` | Host port mapped to the Kafka listener | `9092` |
//! | `STREAMLINE_TEST_HTTP_ENDPOINT` | Full HTTP endpoint override | `http://127.0.0.1:<http-port>` |
//! | `STREAMLINE_TEST_KAFKA_ENDPOINT` | Full `host:port` Kafka override | `127.0.0.1:<kafka-port>` |
//! | `STREAMLINE_TEST_TIMEOUT_SECS` | Per-request/connect bound | `15` |

// unwrap/expect are acceptable in tests; the crate-wide lint targets production code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

/// Default Streamline server image. Kept in sync with `docker-compose.test.yml`
/// by [`compose_default_image_matches_config`].
pub const DEFAULT_IMAGE: &str = "ghcr.io/streamlinelabs/streamline:0.2.0";
const DEFAULT_HTTP_PORT: &str = "9094";
const DEFAULT_KAFKA_PORT: &str = "9092";
const DEFAULT_TIMEOUT_SECS: u64 = 15;

/// Resolved endpoints and bounds for the gated integration suite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationConfig {
    pub image: String,
    pub http_endpoint: String,
    pub kafka_endpoint: String,
    pub timeout_secs: u64,
}

impl IntegrationConfig {
    /// Resolve configuration from the process environment.
    pub fn from_env() -> Self {
        Self::resolve(|key| std::env::var(key).ok())
    }

    /// Resolve configuration from an arbitrary lookup.
    ///
    /// Taking the lookup as a parameter keeps the hermetic tests below free of
    /// global `set_var` mutation, which races across parallel test threads.
    pub fn resolve(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let non_empty = |key: &str| lookup(key).filter(|v| !v.trim().is_empty());

        let http_port =
            non_empty("STREAMLINE_TEST_HTTP_PORT").unwrap_or_else(|| DEFAULT_HTTP_PORT.to_string());
        let kafka_port = non_empty("STREAMLINE_TEST_KAFKA_PORT")
            .unwrap_or_else(|| DEFAULT_KAFKA_PORT.to_string());

        Self {
            image: non_empty("STREAMLINE_TEST_IMAGE").unwrap_or_else(|| DEFAULT_IMAGE.to_string()),
            http_endpoint: non_empty("STREAMLINE_TEST_HTTP_ENDPOINT")
                .unwrap_or_else(|| format!("http://127.0.0.1:{http_port}")),
            kafka_endpoint: non_empty("STREAMLINE_TEST_KAFKA_ENDPOINT")
                .unwrap_or_else(|| format!("127.0.0.1:{kafka_port}")),
            timeout_secs: non_empty("STREAMLINE_TEST_TIMEOUT_SECS")
                .and_then(|v| v.parse().ok())
                .filter(|secs| *secs > 0)
                .unwrap_or(DEFAULT_TIMEOUT_SECS),
        }
    }

    /// Upper bound applied to every networked assertion.
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs)
    }

    fn url(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.http_endpoint.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }
}

// ---------------------------------------------------------------------------
// Hermetic tests — no Kubernetes, no Streamline, no Docker required
// ---------------------------------------------------------------------------

#[test]
fn defaults_resolve_without_any_environment() {
    let config = IntegrationConfig::resolve(|_| None);

    assert_eq!(config.image, DEFAULT_IMAGE);
    assert_eq!(config.http_endpoint, "http://127.0.0.1:9094");
    assert_eq!(config.kafka_endpoint, "127.0.0.1:9092");
    assert_eq!(config.timeout(), Duration::from_secs(DEFAULT_TIMEOUT_SECS));
}

#[test]
fn image_and_endpoints_are_configurable() {
    let config = IntegrationConfig::resolve(|key| match key {
        "STREAMLINE_TEST_IMAGE" => Some("registry.internal/streamline:9.9.9".to_string()),
        "STREAMLINE_TEST_HTTP_ENDPOINT" => Some("https://streamline.internal:8443".to_string()),
        "STREAMLINE_TEST_KAFKA_ENDPOINT" => Some("streamline.internal:19092".to_string()),
        "STREAMLINE_TEST_TIMEOUT_SECS" => Some("42".to_string()),
        _ => None,
    });

    assert_eq!(config.image, "registry.internal/streamline:9.9.9");
    assert_eq!(config.http_endpoint, "https://streamline.internal:8443");
    assert_eq!(config.kafka_endpoint, "streamline.internal:19092");
    assert_eq!(config.timeout(), Duration::from_secs(42));
}

#[test]
fn ports_override_endpoint_defaults() {
    let config = IntegrationConfig::resolve(|key| match key {
        "STREAMLINE_TEST_HTTP_PORT" => Some("18094".to_string()),
        "STREAMLINE_TEST_KAFKA_PORT" => Some("18092".to_string()),
        _ => None,
    });

    assert_eq!(config.http_endpoint, "http://127.0.0.1:18094");
    assert_eq!(config.kafka_endpoint, "127.0.0.1:18092");
}

#[test]
fn blank_and_invalid_values_fall_back_to_defaults() {
    let config = IntegrationConfig::resolve(|key| match key {
        "STREAMLINE_TEST_IMAGE" => Some("   ".to_string()),
        "STREAMLINE_TEST_TIMEOUT_SECS" => Some("not-a-number".to_string()),
        _ => None,
    });

    assert_eq!(config.image, DEFAULT_IMAGE);
    assert_eq!(config.timeout_secs, DEFAULT_TIMEOUT_SECS);

    // A zero timeout would make every bounded assertion fail instantly.
    let zeroed = IntegrationConfig::resolve(|key| {
        (key == "STREAMLINE_TEST_TIMEOUT_SECS").then(|| "0".to_string())
    });
    assert_eq!(zeroed.timeout_secs, DEFAULT_TIMEOUT_SECS);
}

#[test]
fn url_join_is_stable_regardless_of_slashes() {
    let config = IntegrationConfig::resolve(|key| {
        (key == "STREAMLINE_TEST_HTTP_ENDPOINT").then(|| "http://127.0.0.1:9094/".to_string())
    });

    assert_eq!(config.url("/health"), "http://127.0.0.1:9094/health");
    assert_eq!(config.url("health"), "http://127.0.0.1:9094/health");
}

/// The compose file and this harness must agree on the default image, otherwise
/// `make integration-up` and `make test-integration` target different servers.
#[test]
fn compose_default_image_matches_config() {
    let compose = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/docker-compose.test.yml"
    ))
    .expect("docker-compose.test.yml must exist");

    let expected = format!("${{STREAMLINE_TEST_IMAGE:-{DEFAULT_IMAGE}}}");
    assert!(
        compose.contains(&expected),
        "docker-compose.test.yml must make the server image configurable as `{expected}`"
    );
    assert!(
        compose.contains("${STREAMLINE_TEST_HTTP_PORT:-9094}")
            && compose.contains("${STREAMLINE_TEST_KAFKA_PORT:-9092}"),
        "docker-compose.test.yml must make host ports configurable"
    );
}

// ---------------------------------------------------------------------------
// Gated tests — require the services from docker-compose.test.yml
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires a live Streamline server: make integration-up"]
async fn streamline_http_api_is_reachable() {
    let config = IntegrationConfig::from_env();

    let client = reqwest::Client::builder()
        .timeout(config.timeout())
        .build()
        .expect("failed to build HTTP client");

    let response = tokio::time::timeout(config.timeout(), client.get(config.url("/health")).send())
        .await
        .unwrap_or_else(|_| {
            panic!(
                "timed out after {:?} waiting for {}",
                config.timeout(),
                config.url("/health")
            )
        })
        .unwrap_or_else(|e| panic!("request to {} failed: {e}", config.url("/health")));

    assert!(
        response.status().is_success(),
        "expected a successful /health response from {}, got HTTP {}",
        config.http_endpoint,
        response.status()
    );
}

#[tokio::test]
#[ignore = "requires a live Streamline server: make integration-up"]
async fn streamline_kafka_listener_accepts_connections() {
    let config = IntegrationConfig::from_env();

    let stream = tokio::time::timeout(
        config.timeout(),
        tokio::net::TcpStream::connect(&config.kafka_endpoint),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "timed out after {:?} connecting to {}",
            config.timeout(),
            config.kafka_endpoint
        )
    });

    assert!(
        stream.is_ok(),
        "expected the Kafka listener at {} to accept a TCP connection",
        config.kafka_endpoint
    );
}
