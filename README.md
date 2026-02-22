# Streamline Operator

[![CI](https://github.com/streamlinelabs/streamline-operator/actions/workflows/ci.yml/badge.svg)](https://github.com/streamlinelabs/streamline-operator/actions/workflows/ci.yml)
[![codecov](https://img.shields.io/codecov/c/github/streamlinelabs/streamline-operator?style=flat-square)](https://codecov.io/gh/streamlinelabs/streamline-operator)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)
[![Rust](https://img.shields.io/badge/Rust-1.80%2B-orange.svg)](https://www.rust-lang.org/)
[![Kubernetes](https://img.shields.io/badge/Kubernetes-1.26+-326CE5.svg)](https://kubernetes.io/)
[![Docs](https://img.shields.io/badge/docs-streamlinelabs.dev-brightgreen)](https://josedab.github.io/streamline/docs/operations/kubernetes)

Kubernetes operator for managing [Streamline](https://github.com/streamlinelabs/streamline) clusters, topics, and users using Custom Resource Definitions (CRDs).

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
│  StatefulSets     Topic CRUD via   Secrets &        │
│  Services         Streamline API   Credentials      │
│  ConfigMaps                                         │
│  PVCs                                               │
└─────────────────────────────────────────────────────┘
         │                │               │
         ▼                ▼               ▼
┌─────────────────────────────────────────────────────┐
│              Kubernetes Cluster                      │
│  ┌────────────────┐  ┌───────┐  ┌────────────────┐  │
│  │StreamlineCluster│ │Topics │  │StreamlineUsers │  │
│  │  (StatefulSet) │  │(CRDs) │  │  (Secrets)     │  │
│  └────────────────┘  └───────┘  └────────────────┘  │
└─────────────────────────────────────────────────────┘
```

The operator runs three concurrent controllers using [kube-rs](https://kube.rs/), each watching its own CRD and reconciling state with a 30-second requeue interval. All controllers use finalizer-based cleanup for safe deletion.

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

> **Note:** The CRD YAMLs in `deploy/crds/` are hand-maintained to match the
> Rust struct definitions in `src/crd/`. When modifying CRD fields, update both
> the Rust structs and the corresponding YAML schema. A future release will
> auto-generate CRDs from the `kube-derive` annotations at build time.

### Deploy a Streamline Cluster

```yaml
# streamline-cluster.yaml
apiVersion: streaming.streamlinelabs.dev/v1alpha1
kind: StreamlineCluster
metadata:
  name: my-cluster
  namespace: default
spec:
  replicas: 3
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
  tls:
    enabled: false
  podAntiAffinity: true
```

```bash
kubectl apply -f streamline-cluster.yaml
kubectl get streamlineclusters
```

### Create a Topic

```yaml
# my-topic.yaml
apiVersion: streaming.streamlinelabs.dev/v1alpha1
kind: StreamlineTopic
metadata:
  name: events
  namespace: default
spec:
  clusterRef: my-cluster
  partitions: 6
  replicationFactor: 2
  retention:
    ms: 604800000       # 7 days
    bytes: -1           # unlimited
  compression: lz4
  config:
    minInsyncReplicas: 2
```

```bash
kubectl apply -f my-topic.yaml
kubectl get streamlinetopics
```

### Create a User with ACLs

```yaml
# app-user.yaml
apiVersion: streaming.streamlinelabs.dev/v1alpha1
kind: StreamlineUser
metadata:
  name: app-producer
  namespace: default
spec:
  clusterRef: my-cluster
  authentication:
    type: scram-sha-512
    credentials:
      secretRef: app-producer-credentials
  acls:
    - resource: Topic
      name: events
      patternType: literal
      operations: [Write, Describe]
      effect: allow
  quotas:
    producerByteRate: 10485760  # 10MB/s
```

```bash
kubectl apply -f app-user.yaml
kubectl get streamlineusers
```

## Custom Resource Definitions

### StreamlineCluster

Manages a StatefulSet-based Streamline cluster with headless services, persistent storage, and optional TLS.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `spec.replicas` | int | 3 | Number of broker instances |
| `spec.image` | string | latest | Container image |
| `spec.storage.size` | string | 10Gi | PVC size per broker |
| `spec.storage.storageClassName` | string | — | Storage class |
| `spec.tls.enabled` | bool | false | Enable TLS/mTLS |
| `spec.resources` | ResourceRequirements | — | CPU/memory limits |
| `spec.podAntiAffinity` | bool | true | Spread across nodes |
| `spec.rackAwareness.enabled` | bool | false | Zone-aware placement |
| `spec.autoscaling` | AutoScalingSpec | — | Optional HPA config |

**Status phases**: `Pending` → `Running` → `Scaling` / `Upgrading` → `Failed` / `Terminating`

### StreamlineTopic

Manages topics within a referenced Streamline cluster.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `spec.clusterRef` | string | — | Parent cluster name (required) |
| `spec.partitions` | int | 3 | Partition count |
| `spec.replicationFactor` | int | 2 | Replication factor |
| `spec.retention.ms` | int | 604800000 | Retention time (7 days) |
| `spec.retention.bytes` | int | -1 | Retention size (-1 = unlimited) |
| `spec.compression` | string | producer | Compression type |
| `spec.config` | map | — | Additional topic config |

### StreamlineUser

Manages authentication credentials, ACLs, and quotas for users.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `spec.clusterRef` | string | — | Parent cluster name (required) |
| `spec.authentication.type` | string | — | scram-sha-256/512, plain, tlsClientAuth |
| `spec.acls` | []ACL | — | Access control entries |
| `spec.quotas` | QuotaSpec | — | Producer/consumer rate limits |

## Development

### Prerequisites

- Rust 1.80+
- Access to a Kubernetes cluster
- `kubectl` configured

### Build

```bash
cargo build -p streamline-operator
```

### Run Locally

```bash
# Run against your current kubeconfig context
cargo run -p streamline-operator -- --namespace default

# With leader election (for HA deployments)
cargo run -p streamline-operator -- --leader-election

# Custom metrics/health ports
cargo run -p streamline-operator -- --metrics-port 8080 --health-port 8081
```

### Test

```bash
cargo test
```

### Lint

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```

## Observability

| Endpoint | Port | Description |
|----------|------|-------------|
| `/metrics` | 8080 | Prometheus metrics |
| `/healthz` | 8081 | Health probe |

## Project Layout

```
streamline-operator/
├── src/
│   ├── main.rs           # Operator entrypoint, CLI args, graceful shutdown
│   ├── lib.rs            # Public API
│   ├── controllers/      # Reconciliation logic
│   │   ├── cluster.rs    # StreamlineCluster → StatefulSet, Service, ConfigMap
│   │   ├── topic.rs      # StreamlineTopic → API calls to Streamline
│   │   └── user.rs       # StreamlineUser → Secrets, credentials
│   ├── crd/              # CRD type definitions (v1alpha1)
│   │   ├── cluster.rs
│   │   ├── topic.rs
│   │   └── user.rs
│   └── error.rs          # Error types
├── deploy/               # Kubernetes manifests
│   ├── namespace.yaml    # streamline-system namespace
│   └── rbac/             # ServiceAccount, ClusterRole, Binding
├── scripts/              # Build & utility scripts
├── Cargo.toml
├── Makefile
└── CHANGELOG.md
```

## Troubleshooting

### Operator not starting

```bash
# Check operator logs
kubectl logs -n streamline-system deployment/streamline-operator

# Verify CRDs are installed
kubectl get crd | grep streamline

# Check RBAC permissions
kubectl auth can-i list streamlineclusters --as=system:serviceaccount:streamline-system:streamline-operator
```

### Cluster stuck in Pending

```bash
# Check cluster status
kubectl describe streamlinecluster my-cluster

# Check StatefulSet status
kubectl get statefulset -l app.kubernetes.io/managed-by=streamline-operator

# Check PVC binding
kubectl get pvc -l app.kubernetes.io/instance=my-cluster
```

### Topic not becoming Ready

```bash
# Check topic status and conditions
kubectl describe streamlinetopic events

# Verify the parent cluster is running
kubectl get streamlinecluster my-cluster -o jsonpath='{.status.phase}'
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup and guidelines.

## License

Apache License 2.0 — see [LICENSE](LICENSE) for details.
<!-- fix: 0cd8410e -->
