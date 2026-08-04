# Environment Variables

The Streamline Operator supports the following environment variables:

| Variable | Description | Default |
|----------|-------------|---------|
| `STREAMLINE_OPERATOR_NAMESPACE` | Namespace to watch | All namespaces |
| `STREAMLINE_OPERATOR_LOG_LEVEL` | Log verbosity | `info` |
| `STREAMLINE_OPERATOR_METRICS_PORT` | Prometheus metrics port | `8080` |
| `STREAMLINE_OPERATOR_LEADER_ELECTION` | Enable leader election | `true` |
| `STREAMLINE_DEFAULT_IMAGE` | Default Streamline image | `streamlinelabs/streamline:latest` |
| `STREAMLINE_RECONCILE_INTERVAL` | Reconciliation interval | `30s` |

## Integration Test Variables

These apply only to the opt-in integration suite (`tests/integration.rs` and
`docker-compose.test.yml`). They are never read by the operator at runtime, and
the default `cargo test` run ignores them entirely.

| Variable | Description | Default |
|----------|-------------|---------|
| `STREAMLINE_TEST_IMAGE` | Streamline server image started by `docker-compose.test.yml` | `ghcr.io/streamlinelabs/streamline:0.2.0` |
| `STREAMLINE_TEST_HTTP_PORT` | Host port mapped to the server HTTP API | `9094` |
| `STREAMLINE_TEST_KAFKA_PORT` | Host port mapped to the server Kafka listener | `9092` |
| `STREAMLINE_TEST_HTTP_ENDPOINT` | Full HTTP endpoint override for the tests | `http://127.0.0.1:$STREAMLINE_TEST_HTTP_PORT` |
| `STREAMLINE_TEST_KAFKA_ENDPOINT` | Full `host:port` Kafka override for the tests | `127.0.0.1:$STREAMLINE_TEST_KAFKA_PORT` |
| `STREAMLINE_TEST_TIMEOUT_SECS` | Upper bound on every networked assertion | `15` |
| `STREAMLINE_TEST_LOG_LEVEL` | Log verbosity of the containerised server | `info` |

Blank, zero, or unparseable values fall back to the defaults above.

```bash
make integration-up
make test-integration
make integration-down
```
