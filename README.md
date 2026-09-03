# Streamline Operator

[![CI](https://github.com/streamlinelabs/streamline-operator/actions/workflows/ci.yml/badge.svg)](https://github.com/streamlinelabs/streamline-operator/actions/workflows/ci.yml)
[![codecov](https://img.shields.io/codecov/c/github/streamlinelabs/streamline-operator?style=flat-square)](https://codecov.io/gh/streamlinelabs/streamline-operator)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)
[![Rust](https://img.shields.io/badge/Rust-1.88%2B-orange.svg)](https://www.rust-lang.org/)
[![Kubernetes](https://img.shields.io/badge/Kubernetes-1.26+-326CE5.svg)](https://kubernetes.io/)
[![Docs](https://img.shields.io/badge/docs-streamlinelabs.dev-brightgreen)](https://streamlinelabs.dev/docs/operations/kubernetes)
[![Release](https://img.shields.io/github/v/release/streamlinelabs/streamline-operator?label=release)](https://github.com/streamlinelabs/streamline-operator/releases)

Kubernetes operator for managing [Streamline](https://github.com/streamlinelabs/streamline) clusters, topics, and users using Custom Resource Definitions (CRDs).

## ⚠️ Upgrading from v0.3.0 — patch your resources first

**If you have resources created against the v0.3.0 CRDs, patch them before
deploying this operator.** v0.3.0 declared schema defaults that the API server
persisted into every stored object, and this operator rejects all six rather
than run unsafe independent brokers, claim durability the server does not
provide, or advertise topic settings the broker never receives:

| Kind | Field | v0.3.0 persisted | Required now |
|------|-------|------------------|--------------|
| `StreamlineCluster` | `spec.replicas` | `spec.replicas: 3` | `spec.replicas: 1` |
| `StreamlineCluster` | `spec.podAntiAffinity` | `spec.podAntiAffinity: true` | `spec.podAntiAffinity: false` |
| `StreamlineTopic` | `spec.replicationFactor` | `spec.replicationFactor: 2` | `spec.replicationFactor: 1` |
| `StreamlineTopic` | `spec.retention.retentionMs` | `spec.retention.retentionMs: 604800000` | `spec.retention.retentionMs: -1` |
| `StreamlineTopic` | `spec.config.minInsyncReplicas` | `spec.config.minInsyncReplicas: 1` | `spec.config.minInsyncReplicas: null` (remove) |
| `StreamlineTopic` | `spec.config.maxMessageBytes` | `spec.config.maxMessageBytes: 1048576` | `spec.config.maxMessageBytes: null` (remove) |

Your manifests may never mention these fields — Kubernetes applies CRD defaults
on write, so the values are in etcd regardless, and installing the corrected
CRDs does **not** rewrite objects that already exist. The three top-level fields
are in every v0.3.0 resource; the three nested ones are in every topic whose
manifest opened a `retention:` or `config:` block, because defaulting only
descends into objects that are present. Nothing is auto-mutated: an unpatched
resource reports `phase: Failed` (`InvalidSpec` / `UnsupportedConfiguration`)
and stops reconciling, while its existing workloads keep running untouched.

```bash
# Find every affected resource (v0.3.0 ignored --namespace and watched them all)
kubectl get streamlineclusters --all-namespaces \
  -o custom-columns='NAMESPACE:.metadata.namespace,NAME:.metadata.name,REPLICAS:.spec.replicas,PODANTIAFFINITY:.spec.podAntiAffinity'
kubectl get streamlinetopics --all-namespaces \
  -o custom-columns='NAMESPACE:.metadata.namespace,NAME:.metadata.name,REPLICATIONFACTOR:.spec.replicationFactor,RETENTIONMS:.spec.retention.retentionMs,CONFIG:.spec.config'

# Install the corrected CRDs FIRST: v0.3.0 defaults the two spec.config keys, so
# a removal applied while those CRDs are installed is undone on the same write.
kubectl apply -k deploy/crds/

# Then patch each resource, in the namespace it lives in
kubectl patch streamlineclusters my-cluster -n streamline-system --type merge -p '{"spec":{"replicas":1}}'
kubectl patch streamlineclusters my-cluster -n streamline-system --type merge -p '{"spec":{"podAntiAffinity":false}}'
kubectl patch streamlinetopics events -n streamline-system --type merge -p '{"spec":{"replicationFactor":1}}'
kubectl patch streamlinetopics events -n streamline-system --type merge -p '{"spec":{"retention":{"retentionMs":-1}}}'
kubectl patch streamlinetopics events -n streamline-system --type merge -p '{"spec":{"config":{"minInsyncReplicas":null}}}'
kubectl patch streamlinetopics events -n streamline-system --type merge -p '{"spec":{"config":{"maxMessageBytes":null}}}'
```

Two fields v0.3.0's CRDs advertised are **gone** from these schemas and are
pruned by the API server on the next write: `StreamlineCluster.spec.replication`
(never read by any v0.3.0 code path) and `StreamlineTopic.spec.config.flushMs`
(superseded by `flushIntervalMs`, which the operator also rejects). Copy
anything you still need out of them before installing the CRDs.

📖 **[docs/UPGRADING.md](docs/UPGRADING.md)** has the full path: discovery across
every namespace, `--dry-run=server` rehearsal, bulk patch loops, verification,
what the rejection messages look like, what happens to the data on brokers
`-1` and `-2` when a cluster scales back to one replica, and what a rollback
does and does not restore.

## Architecture

```
┌─────────────────────────────────────────────────────┐
│                 Streamline Operator                  │
│                                                     │
│  ┌─────────────┐ ┌─────────────┐ ┌──────────────┐  │
│  │   Cluster   │ │    Topic    │ │     User     │  │
│  │ Controller  │ │ Controller  │ │  Controller  │  │
│  └──────┬──────┘ └──────┬──────┘ └──────┬───────┘  │
│         │               │               │           │
│         ▼               ▼               ▼           │
│  StatefulSets     Topic create/    Unsupported:     │
│  Services         delete via the   reports status,  │
│  ConfigMaps       Streamline API   creates nothing  │
│  PVCs                                               │
└─────────────────────────────────────────────────────┘
         │                │               │
         ▼                ▼               ▼
┌─────────────────────────────────────────────────────┐
│              Kubernetes Cluster                      │
│  ┌────────────────┐  ┌───────┐  ┌────────────────┐  │
│  │StreamlineCluster│ │Topics │  │StreamlineUsers │  │
│  │  (StatefulSet) │  │(CRDs) │  │ (status only)  │  │
│  └────────────────┘  └───────┘  └────────────────┘  │
└─────────────────────────────────────────────────────┘
```

The operator runs three concurrent controllers using [kube-rs](https://kube.rs/)
— `StreamlineCluster`, `StreamlineTopic`, and `StreamlineUser` — each watching
its own CRD. Controllers that create Kubernetes objects use finalizer-based
cleanup for safe deletion.

`StreamlineBranch`, `StreamlineContract`, `StreamlineMemory`,
`StreamlineBackup`, and `StreamlineEdge` have **no controller** and are **not
installed**; see [Schema-only CRDs](#schema-only-crds).

### Namespace scope and RBAC

By default the shipped `deploy/operator.yaml` passes
`--namespace=$(OPERATOR_NAMESPACE)`, so the operator watches only the namespace
it runs in, and `deploy/rbac/` grants a namespaced **Role**/**RoleBinding**.

For a cloud control plane that must reconcile tenant resources across
namespaces, use the opt-in `overlays/cloud/` Kustomize overlay. It patches the
Deployment to pass `--namespace=` (an explicit empty value), binds the
ServiceAccount to a least-privilege `ClusterRole`, and keeps leader-election
Lease access in a namespaced `Role`. It also labels the `streamline-system`
namespace with `streamline.io/control-plane=true`; Streamline Cloud's tenant
NetworkPolicies require that label before the topic controller can reach the
private broker HTTP API on port 9094. The default `deploy/` install remains
namespace-scoped and does not carry the cross-namespace access label.

The leader-election Lease always lives in the namespace the operator *runs* in
(or `--leader-election-namespace`), independently of what it watches.

### Clustering status

The cluster controller renders each replica as a **standalone broker**: it does
not bootstrap raft peers, so `spec.replicas` defaults to `1` and values greater
than `1` are rejected as unsupported rather than producing independent brokers
that look like a quorum.

The v0.3.0 CRD defaulted `spec.replicas` to `3`, so resources created against it
are rejected until patched — see
[Upgrading from v0.3.0](#️-upgrading-from-v030--patch-your-resources-first).

### User management is not supported

The Streamline server exposes no user API, so `StreamlineUser` resources are
reported as `Unsupported`: no user, ACL, quota, or credentials Secret is
created. The CRD remains installed so existing manifests surface an explicit
status instead of silently doing nothing.

## Quick Start

### Prerequisites

- Kubernetes 1.26+ cluster (kind, minikube, GKE, EKS, AKS)
- `kubectl` configured with valid kubeconfig
- [Helm 3.x](https://helm.sh/) (for Helm-based install) or `kubectl` (for manifest-based install)

### Install via Helm (Recommended)

```bash
helm repo add streamline https://streamlinelabs.github.io/charts
helm install streamline-operator streamline/streamline-operator \
  --namespace streamline-system --create-namespace
```

### Install via Manifests

```bash
# Install CRDs first
kubectl apply -k deploy/crds/

# Install RBAC and operator
kubectl apply -f deploy/namespace.yaml
kubectl apply -f deploy/rbac/
kubectl apply -f deploy/operator.yaml
```

With kustomize, set the image in your own overlay instead:

```bash
kustomize edit set image \
  ghcr.io/streamlinelabs/streamline-operator=ghcr.io/streamlinelabs/streamline-operator@sha256:<digest>
kubectl apply -k deploy/
```

For the cluster-wide cloud mode, apply the dedicated overlay after setting the
same immutable image in that overlay (or in a downstream overlay that imports
it):

```bash
(cd overlays/cloud && kustomize edit set image \
  ghcr.io/streamlinelabs/streamline-operator=ghcr.io/streamlinelabs/streamline-operator@sha256:<digest>)
kubectl apply -k overlays/cloud/
```

This is an explicit privilege expansion: reconciliation is cluster-wide, while
the leader-election Lease remains restricted to `streamline-system`.

> **Note:** The CRD YAMLs in `deploy/crds/` are generated from the
> `#[kube(...)]` annotations in `src/crd/` — regenerate them with
> `make generate-crds` (or `streamline-operator --generate-crds`) and never edit
> them by hand. `cargo test` fails if the checked-in manifests drift from the
> Rust types, and the release pipeline verifies the same invariant.
>
> Only CRDs that a controller reconciles are installed — `StreamlineCluster`,
> `StreamlineTopic`, and `StreamlineUser`. See
> [Schema-only CRDs](#schema-only-crds) for the rest.

> **Every example below is in `streamline-system`.** The shipped Deployment
> passes `--namespace=$(OPERATOR_NAMESPACE)` and `deploy/rbac/` grants a
> namespaced Role in `streamline-system`, so that is the only namespace the
> operator watches and the only one it is authorised to read. A custom resource
> created anywhere else — `default` included — is never reconciled and reports
> no status at all. To use another namespace, deploy the operator there (the
> watch follows the Deployment) or opt into cluster-wide mode as described in
> [Namespace scope and RBAC](#namespace-scope-and-rbac).

### Deploy a Streamline Cluster

```yaml
# streamline-cluster.yaml
apiVersion: streamline.io/v1alpha1
kind: StreamlineCluster
metadata:
  name: my-cluster
  namespace: streamline-system
spec:
  replicas: 1
  image: ghcr.io/streamlinelabs/streamline:latest
  storage:
    size: 10Gi
    storageClassName: standard
  resources:
    requests:
      cpu: 500m
      memory: 512Mi
    limits:
      cpu: "2"
      memory: 2Gi
```

TLS is off unless `spec.tls` is present, and enabling it requires
`spec.tls.secretName` (plus `caSecretName` for mTLS) — the operator rejects a
`tls` block it cannot mount.

```bash
kubectl apply -f streamline-cluster.yaml
kubectl get streamlineclusters -n streamline-system
```

### Create a Topic

```yaml
# my-topic.yaml
apiVersion: streamline.io/v1alpha1
kind: StreamlineTopic
metadata:
  name: events
  namespace: streamline-system
spec:
  clusterRef: my-cluster
  partitions: 6
  replicationFactor: 1
```

```bash
kubectl apply -f my-topic.yaml
kubectl get streamlinetopics -n streamline-system
```

> **Only `partitions` is applied.** The Streamline topic API accepts a name and
> a partition count and nothing else: it creates single-replica topics and
> exposes no way to set retention, compaction, compression, or any other topic
> config. Rather than sending settings the server discards and then reporting
> `Ready`, the controller **rejects** any `spec.retention`, `spec.compression`,
> or `spec.config` value that differs from the schema default, and publishes
> `phase: Failed` with an explanation. The `Synced` condition only ever claims
> the partition count and replication factor the server actually reported back.

### Create a User with ACLs

```yaml
# app-user.yaml
apiVersion: streamline.io/v1alpha1
kind: StreamlineUser
metadata:
  name: app-producer
  namespace: streamline-system
spec:
  clusterRef: my-cluster
  authentication:
    type: scram-sha512
    credentials:
      secretRef:
        name: app-producer-credentials
        key: password
  authorization:
    acls:
      - resourceType: topic
        resourceName: events
        patternType: literal
        operations: [write, describe]
        permission: allow
  quotas:
    producerByteRate: 10485760  # 10MB/s
```

```bash
kubectl apply -f app-user.yaml
# Reports phase Unsupported — see "User management is not supported".
kubectl get streamlineusers -n streamline-system
```

## Custom Resource Definitions

### StreamlineCluster

Manages a StatefulSet-based Streamline cluster with headless services, persistent storage, and optional TLS.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `spec.replicas` | int | 1 | Number of broker instances (standalone; see "Clustering status") |
| `spec.image` | string | latest | Container image |
| `spec.storage.size` | string | 10Gi | PVC size per broker |
| `spec.storage.storageClassName` | string | — | Storage class |
| `spec.tls.enabled` | bool | false | Enable TLS; requires `spec.tls.secretName`, which is mounted at `/etc/streamline-tls` |
| `spec.tls.mtlsEnabled` | bool | false | Require client certs; requires `spec.tls.caSecretName`, mounted at `/etc/streamline-tls-ca` |
| `spec.tls.insecureSkipVerify` | bool | false | Rejected — the operator refuses to render it |
| `spec.resources` | ResourceRequirements | — | CPU/memory limits |
| `spec.env` | []EnvVar | — | Extra broker variables; `value` and `valueFrom.secretKeyRef`/`valueFrom.configMapKeyRef` are rendered onto the container, and the kubelet resolves the reference (the operator never reads the Secret) |
| `spec.nodeSelector` | map[string]string | — | Applied — rendered as the pod's `nodeSelector` |
| `spec.podAntiAffinity` | bool | false | Rejected when `true` — the operator renders no affinity rules |
| `spec.rackAwareness.enabled` | bool | false | Rejected when `true` — the operator renders no topology spread or rack labelling |
| `spec.tolerations` | []Toleration | — | Rejected when non-empty — the operator renders no tolerations |
| `spec.autoscaling` | AutoScalingSpec | — | Rejected when enabled until the operator can bootstrap real multi-broker clusters |

`nodeSelector` is the only scheduling control the pod template carries.
`podAntiAffinity`, `rackAwareness`, and `tolerations` remain in the schema for
compatibility, but the controller rejects them with `InvalidSpec` instead of
accepting a placement request it never renders. `podAntiAffinity` defaulted to
`true` in v0.3.0 and was persisted into existing objects, so upgrading
installations must patch it to `false` — see
[docs/UPGRADING.md](docs/UPGRADING.md).

**Status phases**: `Pending` → `Running` → `Scaling` / `Upgrading` → `Failed` / `Terminating`

### StreamlineTopic

Manages topics within a referenced Streamline cluster.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `spec.clusterRef` | string | — | Parent cluster name (required) |
| `spec.partitions` | int | 3 | Partition count — the only field the server applies |
| `spec.replicationFactor` | int | 1 | Replication factor; values other than `1` are rejected as unsupported. Defaulted to `2` in v0.3.0 — see [docs/UPGRADING.md](docs/UPGRADING.md) |
| `spec.retention.retentionMs` | int | -1 | Unlimited. Not applied: the broker never expires a segment on age, so any other value is rejected |
| `spec.retention.retentionBytes` | int | -1 | Unlimited. Not applied; any other value is rejected |
| `spec.retention.cleanupPolicy` | string | `delete` | Not applied; any other value is rejected |
| `spec.compression.type` | string | `producer` | Not applied — compression is chosen by the producer; any other value is rejected |
| `spec.config` | object | — | Topic config overrides; **every** entry is rejected (the server applies none) |

Both retention axes default to `-1` because the core topic API applies no topic
configuration at all: topics are retained indefinitely. `retentionMs` used to
default to `604800000`, which advertised a seven-day policy nothing implemented
— and because non-default values are rejected, that wrong value was also the
only one a user was permitted to keep.

### StreamlineUser

> ⚠️ **Unsupported.** The Streamline server has no user API, so the controller
> publishes an `Unsupported` status and creates nothing (no user, no ACLs, no
> quotas, no credentials Secret). The fields below describe the schema only.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `spec.clusterRef` | string | — | Parent cluster name (required) |
| `spec.authentication.type` | string | — | `scram-sha256`, `scram-sha512`, `plain`, `tls-client-auth` |
| `spec.acls` | []ACL | — | Access control entries |
| `spec.quotas` | QuotaSpec | — | Producer/consumer rate limits |

## Schema-only CRDs

> ⚠️ **Not installed, not reconciled.** The Rust types for the kinds below
> exist so their schemas can be generated and reviewed, and
> `make generate-crds` writes a manifest for each into `deploy/crds/`. None of
> them is listed in `deploy/crds/kustomization.yaml`, none appears in
> `deploy/rbac/role.yaml`, and the operator starts no controller for any of
> them. Applying one of these CRDs by hand gives you a resource nothing will
> ever act on.
>
> Each generated manifest states the specific reason in its header, and
> `cargo test` fails if a controller, CRD installation, or RBAC grant is added
> without changing the reconciliation metadata in `src/crd/generate.rs`.

| Kind | Why it is schema only |
|------|-----------------------|
| `StreamlineBranch` | The server's branch API sits behind the non-default `branches` cargo feature (absent from both the `lite` default and `full`) and takes `{base_topic, name, base_offsets, created_by}` with `<base_topic>:<name>` identifiers — a shape this CRD cannot express. |
| `StreamlineContract` | The server exposes `POST /api/v1/contracts/{validate,apply}` taking `{contract: {topic, version, schema_id, assertions}, partition, value}`. There is no `POST /api/v1/contracts`, and no schema/compatibility/`bindTopics` contract to apply. |
| `StreamlineMemory` | The server's memory API sits behind the non-default `agent-memory` cargo feature and is a remember/recall endpoint, not a provisioning API. A controller could only create ordinary topics and then report memory tiers `Ready` that nothing implements. |
| `StreamlineBackup` | The server exposes no backup or restore API. |
| `StreamlineEdge` | The server's edge sync support sits behind the non-default `edge` cargo feature and exposes no endpoint this CRD could reconcile. |

To read a schema, use the generated manifest rather than a hand-written
example:

```bash
kubectl explain --recursive -f deploy/crds/streamlinebranch-crd.yaml  # offline schema
less deploy/crds/streamlinecontract-crd.yaml
```

## Development

### Prerequisites

- Rust 1.88+
- Access to a Kubernetes cluster
- `kubectl` configured

### Build

```bash
cargo build -p streamline-operator
```

### Run Locally

```bash
# Watch a single namespace (what the shipped Deployment does, and the
# namespace every example in this README uses)
cargo run -p streamline-operator -- --namespace streamline-system

# Watch every namespace when running outside the manifests. The packaged form
# is overlays/cloud/, which supplies the matching cluster-wide RBAC.
cargo run -p streamline-operator -- --namespace=

# With leader election (for HA deployments)
cargo run -p streamline-operator -- --leader-election

# Custom metrics/health bind addresses
cargo run -p streamline-operator -- \
  --metrics-bind-address 0.0.0.0:8080 \
  --health-probe-bind-address 0.0.0.0:8081

# Print the CRD manifests (no cluster required)
cargo run -p streamline-operator -- --generate-crds

# Regenerate deploy/crds/ from the Rust types
make generate-crds
```

### Test

```bash
cargo test
```

The default test run is hermetic: it needs no Kubernetes cluster, no Streamline
server, and no Docker.

#### Integration tests (opt-in)

Tests that need a live Streamline server live in `tests/integration.rs` and are
`#[ignore]`d, so they never run as part of `cargo test`. Every networked
assertion is bounded by a timeout so a missing backend fails fast.

```bash
make integration-up        # start the server from docker-compose.test.yml
make test-integration      # cargo test --test integration -- --ignored
make integration-down
```

The image and endpoints are configurable — see
[`docs/ENVIRONMENT.md`](docs/ENVIRONMENT.md#integration-test-variables):

```bash
STREAMLINE_TEST_IMAGE=ghcr.io/streamlinelabs/streamline:0.3.0 \
STREAMLINE_TEST_HTTP_PORT=19094 \
STREAMLINE_TEST_KAFKA_PORT=19092 make integration-up

STREAMLINE_TEST_HTTP_ENDPOINT=http://127.0.0.1:19094 \
STREAMLINE_TEST_KAFKA_ENDPOINT=127.0.0.1:19092 make test-integration
```

A separate Helm/kubectl suite lives in `scripts/helm-integration-test.sh` and
requires a configured `kubectl` context.

### Lint

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```

## Observability

| Endpoint | Port | Description |
|----------|------|-------------|
| `/metrics` | 8080 | Prometheus metrics |
| `/healthz` | 8081 | Liveness probe — `200` while the process runs |
| `/readyz` | 8081 | Readiness probe — `200` once this operator process is initialised, **including a leader-election standby** |
| `/leaderz` | 8081 | `200` only on the replica currently holding the leader Lease; `503` on standbys |

`/readyz` deliberately does **not** mean "this replica is the leader". A
standby is a healthy operator waiting for the Lease, and failing its readiness
probe would deadlock a rolling update of an HA Deployment: the new pod waits for
the lease the outgoing pod still holds, while the rollout waits for the new pod
to become ready. Use `/leaderz` — or the `streamline_operator_leader` gauge,
which is `1` on exactly one replica — to find or alert on the active operator.

## Project Layout

```
streamline-operator/
├── src/
│   ├── main.rs           # Operator entrypoint, CLI args, graceful shutdown
│   ├── lib.rs            # Public API
│   ├── controllers/      # Reconciliation logic
│   │   ├── cluster.rs    # StreamlineCluster → StatefulSet, Service, ConfigMap
│   │   ├── topic.rs      # StreamlineTopic → API calls to Streamline
│   │   └── user.rs       # StreamlineUser → status only (unsupported)
│   ├── health.rs         # /healthz, /readyz, /leaderz
│   ├── upgrade.rs        # v0.3.0 legacy defaults: rejection messages + patches
│   ├── crd/              # CRD type definitions (v1alpha1)
│   │   ├── cluster.rs
│   │   ├── topic.rs
│   │   └── user.rs
│   └── error.rs          # Error types
├── deploy/               # Kubernetes manifests
│   ├── namespace.yaml    # streamline-system namespace
│   ├── crds/             # Generated CRDs (only reconciled kinds installed)
│   ├── operator.yaml     # Deployment (unpullable placeholder image)
│   └── rbac/             # ServiceAccount, namespaced Role, RoleBinding
├── overlays/
│   └── cloud/            # Opt-in watch-all deployment and cluster-wide RBAC
├── docs/
│   ├── UPGRADING.md      # v0.3.0 → current: required kubectl patches
│   ├── API.md            # CRD field reference
│   ├── ENVIRONMENT.md    # Flags and environment variables
│   └── TROUBLESHOOTING.md
├── scripts/              # Build & utility scripts
├── Cargo.toml
├── Makefile
└── CHANGELOG.md
```

## Troubleshooting

### Cluster or topic reports `Failed` right after an upgrade

A `Failed` phase with `InvalidSpec` on `spec.replicas`/`spec.podAntiAffinity`,
or `UnsupportedConfiguration` on `spec.replicationFactor`, means the resource
still carries a default v0.3.0 persisted into it. Patch it — the message names
the field and the exact command, and
[docs/UPGRADING.md](docs/UPGRADING.md) has the full sequence.

```bash
# Every resource still carrying a rejected v0.3.0 value
kubectl get streamlineclusters --all-namespaces \
  -o jsonpath='{range .items[?(@.spec.replicas>1)]}{.metadata.namespace}{"/"}{.metadata.name}{"\n"}{end}'
kubectl get streamlinetopics --all-namespaces \
  -o jsonpath='{range .items[?(@.spec.replicationFactor!=1)]}{.metadata.namespace}{"/"}{.metadata.name}{"\n"}{end}'
```

### Operator not starting

```bash
# Check operator logs
kubectl logs -n streamline-system deployment/streamline-operator

# Verify CRDs are installed
kubectl get crd | grep streamline

# Check RBAC permissions (namespaced by default)
kubectl auth can-i list streamlineclusters \
  --as=system:serviceaccount:streamline-system:streamline-operator \
  -n streamline-system
```

### Cluster stuck in Pending

```bash
# Check cluster status
kubectl describe streamlinecluster my-cluster -n streamline-system

# Check StatefulSet status
kubectl get statefulset -n streamline-system \
  -l app.kubernetes.io/managed-by=streamline-operator

# Check PVC binding
kubectl get pvc -n streamline-system \
  -l app.kubernetes.io/instance=my-cluster
```

### Topic not becoming Ready

An `Unsupported`/`Failed` phase with a message about settings being "silently
discarded" means the spec sets a topic option the Streamline server does not
apply — remove it (see "Create a Topic").

```bash
# Check topic status and conditions
kubectl describe streamlinetopic events -n streamline-system

# Verify the parent cluster is running
kubectl get streamlinecluster my-cluster -n streamline-system \
  -o jsonpath='{.status.phase}'
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup and guidelines.

## License

Apache License 2.0 — see [LICENSE](LICENSE) for details.
