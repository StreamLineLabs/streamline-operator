# CLAUDE.md — Streamline Kubernetes Operator

## Overview
Kubernetes operator for [Streamline](https://github.com/streamlinelabs/streamline), managing `StreamlineCluster`, `StreamlineTopic`, and `StreamlineUser` CRDs. Built with [kube-rs](https://kube.rs/).

## Build & Test
```bash
cargo build -p streamline-operator    # Build
cargo test                            # Run tests
cargo fmt --all -- --check            # Check formatting
cargo clippy --all-targets -- -D warnings  # Lint
```

## Architecture
```
src/
├── main.rs              # Entrypoint — CLI args, controller startup, graceful shutdown
├── lib.rs               # Public API
├── controllers/         # Reconciliation loops
│   ├── cluster.rs       # StreamlineCluster → StatefulSet, Service, ConfigMap
│   ├── topic.rs         # StreamlineTopic → API calls to Streamline
│   └── user.rs          # StreamlineUser → Secrets, credentials
├── crd/                 # CRD type definitions (v1alpha1)
│   ├── cluster.rs       # StreamlineCluster spec/status
│   ├── topic.rs         # StreamlineTopic spec/status
│   └── user.rs          # StreamlineUser spec/status
├── conditions.rs        # Status condition helpers
├── error.rs             # Error types
└── leader_election.rs   # HA leader lease management
deploy/
├── namespace.yaml       # streamline-system namespace
└── rbac/                # ServiceAccount, ClusterRole, Binding
```

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
