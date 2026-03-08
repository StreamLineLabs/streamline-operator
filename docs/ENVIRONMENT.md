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
