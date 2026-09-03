# CLAUDE.md — Streamline Kubernetes Operator

## Overview
Kubernetes operator for [Streamline](https://github.com/streamlinelabs/streamline), managing `StreamlineCluster`, `StreamlineTopic`, and `StreamlineUser` CRDs. Built with [kube-rs](https://kube.rs/).

## Build & Test
```bash
cargo build -p streamline-operator    # Build
cargo test                            # Run tests (hermetic)
make generate-crds                    # Regenerate deploy/crds/ from src/crd/
cargo fmt --all -- --check            # Check formatting
cargo clippy --all-targets -- -D warnings  # Lint
```

## Architecture
```
src/
├── main.rs              # Entrypoint — CLI args, controller startup, graceful shutdown
├── lib.rs               # Public API
├── controllers/         # Reconciliation loops (exactly three; see boundaries)
│   ├── mod.rs           # WatchScope (--namespace), shared error policy
│   ├── cluster.rs       # StreamlineCluster → StatefulSet, Service, ConfigMap
│   ├── topic.rs         # StreamlineTopic → API calls to Streamline
│   └── user.rs          # StreamlineUser → status only (unsupported)
├── crd/                 # CRD type definitions (v1alpha1)
│   ├── cluster.rs       # StreamlineCluster spec/status
│   ├── topic.rs         # StreamlineTopic spec/status
│   └── user.rs          # StreamlineUser spec/status
├── conditions.rs        # Status condition helpers
├── error.rs             # Error types
├── health.rs            # /healthz + /readyz + /leaderz probe state and router
├── crd/generate.rs      # Deterministic CRD manifest generation
└── leader_election.rs   # HA leader lease management
deploy/
├── namespace.yaml       # streamline-system namespace
└── rbac/                # ServiceAccount, ClusterRole, Binding
```

## CRD Manifests
`deploy/crds/*.yaml` is generated — run `make generate-crds` after changing any
type in `src/crd/`. `cargo test` fails on drift.

## Hermetic Static Suites
`tests/crd_manifests.rs` (CRD drift, install/RBAC/controller agreement),
`tests/static_manifests.rs` (Dockerfile, workflows, manifests, image gating,
namespace scope, probe ordering), and `tests/docs_examples.rs` (every fenced
YAML example in `README.md`, `docs/API.md`, and `src/lib.rs` validated against
the generated CRD schemas). `make static` runs all three.

## Server Support Boundaries
- `StreamlineUser` is unsupported: the server has no user API, so the controller
  reports `Unsupported` and creates nothing.
- Topics are single-replica; `replicationFactor != 1` is rejected.
- The topic API accepts only `{name, partitions}`. Every `spec.retention`,
  `spec.compression`, and `spec.config` value that differs from the schema
  default is **rejected** rather than sent and discarded, and the `Synced`
  condition only ever claims the partition count and replication factor the
  server reported back.
- Clusters render standalone brokers (no raft bootstrap); `replicas` defaults to 1.
- `spec.nodeSelector` is the only scheduling control the pod template renders.
  `spec.podAntiAffinity` (default `false`), `spec.rackAwareness.enabled`, and a
  non-empty `spec.tolerations` are **rejected**: nothing renders affinity,
  topology spread, or tolerations, so accepting them would report `Ready` for a
  placement that never happened.
- `spec.env[].valueFrom` supports `secretKeyRef` and `configMapKeyRef`, mapped
  verbatim onto the container's `EnvVarSource` (the kubelet resolves them, so
  the operator needs no Secret RBAC). Entries that cannot be mapped exactly are
  rejected; the renderer never emits a variable it cannot source.
- Cluster autoscaling is rejected until raft peer bootstrap exists; an HPA
  must not create multiple independent brokers. Every cluster reconcile deletes
  any operator-owned HPA when autoscaling is absent, disabled, or invalid.
- `StreamlineBranch`, `StreamlineContract`, `StreamlineMemory`,
  `StreamlineBackup`, and `StreamlineEdge` are **schema-only**: no controller,
  not installed by `deploy/crds/kustomization.yaml`, no RBAC. The reason per
  kind lives in `Reconciliation::None(..)` in `src/crd/generate.rs` and is
  rendered into each generated manifest. `cargo test` fails if one is
  re-enabled without changing that metadata.

## Deployment Boundaries
- `deploy/operator.yaml` ships
  `ghcr.io/streamlinelabs/streamline-operator:REPLACE_WITH_RELEASED_IMAGE` with
  `imagePullPolicy: Always`. It is intentionally unpullable so the checked-in
  manifests can never silently run an operator older than the tree. Render a
  deployable manifest with `make release-manifests IMAGE=<repo>@sha256:<digest>`.
- The Deployment passes `--namespace=$(OPERATOR_NAMESPACE)` and `deploy/rbac/`
  ships a namespaced `Role`/`RoleBinding`. The opt-in `overlays/cloud/`
  Kustomize overlay passes `--namespace=`, grants reconciliation through a
  least-privilege `ClusterRole`/`ClusterRoleBinding`, keeps Lease access in the
  operator namespace, and labels that namespace
  `streamline.io/control-plane=true` so Cloud tenant NetworkPolicies admit the
  operator's HTTP 9094 reconciliation traffic.
- The operator holds **no** Secret RBAC: TLS material is mounted through a
  `SecretVolumeSource` that the kubelet reads, and no credentials are created.
- `/readyz` means "this operator process is healthy", including a
  leader-election standby. Leadership is `/leaderz` and the
  `streamline_operator_leader` gauge.

## Coding Conventions
- **No `.unwrap()` in production**: `#[warn(clippy::unwrap_used)]` enforced
- **Async**: Tokio runtime, all controllers are async
- **Finalizers**: All CRD controllers use finalizer-based cleanup
- **Status patching**: Use JSON patch for status subresource updates
- **Logging**: Structured JSON logging via `tracing`

## Key Patterns
- Controllers requeue on error with 30-second interval
- Leader election via Kubernetes Leases for HA deployments
- CRD status tracks: phase, conditions (Ready, Progressing, Degraded), error messages

## Dependencies
- `kube` 0.95 — Kubernetes client + runtime
- `k8s-openapi` 0.23 — Kubernetes API types (v1_30)
- `tokio` 1.41 — Async runtime
- `clap` — CLI argument parsing
