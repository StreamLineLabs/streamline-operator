# Environment Variables

## Operator runtime

The operator is configured with CLI flags; only the variables below are read
from the environment.

| Variable | Description | Default |
|----------|-------------|---------|
| `RUST_LOG` | `tracing` filter, e.g. `info,streamline_operator=debug` | `info` |
| `POD_NAME` | Leader-election identity (falls back to `HOSTNAME`) | container hostname |
| `HOSTNAME` | Leader-election identity fallback | — |

Everything else is a flag:

| Flag | Description | Default |
|------|-------------|---------|
| `--namespace` | Namespace to watch for `StreamlineCluster`/`StreamlineTopic`/`StreamlineUser` (empty = all namespaces) | all namespaces |
| `--leader-election` | Enable lease-based leader election | `false` |
| `--leader-election-namespace` | Lease namespace (auto-detected from the service account when empty) | auto |
| `--metrics-bind-address` | Prometheus metrics listener | `0.0.0.0:8080` |
| `--health-probe-bind-address` | `/healthz` + `/readyz` + `/leaderz` listener | `0.0.0.0:8081` |
| `--generate-crds` | Print the installed CRD manifests to stdout and exit | — |
| `--generate-crds-dir <DIR>` | Write one CRD manifest per kind into `DIR` and exit | — |

`--namespace` scopes every enabled controller's watch. The shipped
`deploy/operator.yaml` passes `--namespace=$(OPERATOR_NAMESPACE)`, which
Kubernetes expands from the downward-API environment variable of the same name,
so the operator watches only its own namespace and the namespaced
`Role`/`RoleBinding` in `deploy/rbac/` is sufficient. Leaving `--namespace`
empty selects cluster-wide mode. The opt-in `overlays/cloud/` Kustomize overlay
supplies the matching `ClusterRole`/`ClusterRoleBinding` while retaining a
namespaced Role for the leader-election Lease. It also labels the operator
namespace `streamline.io/control-plane=true`, which is the selector Streamline
Cloud's tenant NetworkPolicies require for private HTTP 9094 access.

`--leader-election-namespace` is independent of `--namespace`: the Lease lives
where the operator *runs*, not where it watches. When it is empty the namespace
is read from the projected service account token, so the operator needs no
`namespaces` RBAC.

Both `--generate-crds` flags are offline: they render the manifests in
`deploy/crds/` from the Rust types and exit before a Kubernetes client is
needed. `make generate-crds` wraps the directory form.

## Broker configuration

The cluster controller renders a TOML file into the `<cluster>-config`
ConfigMap, mounts it read-only at `/etc/streamline`, and points the broker at it
with `STREAMLINE_CONFIG=/etc/streamline/streamline.toml`. The file uses the
server's own configuration schema: `[server]` (`listen_addr`, `http_addr`,
`data_dir`, `log_level`) and, when `spec.tls.enabled` is set, `[tls]`
(`enabled`, `cert`, `key`, and — for mTLS — `require_client_cert`, `ca_cert`).
Keys the server does not define are deliberately not rendered. The matching
`STREAMLINE_DATA_DIR`, `STREAMLINE_LISTEN_ADDR`, `STREAMLINE_HTTP_ADDR`, and
`STREAMLINE_LOG_LEVEL` variables are set on the container as well, so either
mechanism yields the same values. Additional variables can be supplied through
`spec.env`.

When `spec.tls.enabled` is set, the referenced Secrets are mounted **beside**
the config directory, not inside it: the server keypair at
`/etc/streamline-tls` and, for mTLS, the client CA bundle at
`/etc/streamline-tls-ca`. Nesting a Secret volume under the read-only ConfigMap
mount is not a layout kubelet guarantees, and a lost mount would leave the
broker pointed at certificate paths that do not exist. The pod runs with
`fsGroup: 1000` and the Secret files are projected with mode `0440`, so the
non-root broker can read them without making private keys world-readable.

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
