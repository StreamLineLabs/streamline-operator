# Contributing to Streamline Operator

Thank you for your interest in contributing! Please review the [organization-wide contributing guidelines](https://github.com/streamlinelabs/.github/blob/main/CONTRIBUTING.md) first.

## Development Setup

### Prerequisites

- Rust 1.75+ (`rustup update stable`)
- Docker (for integration tests)
- kubectl + a Kubernetes cluster (for e2e tests, [kind](https://kind.sigs.k8s.io/) recommended)

### Build & Test

```bash
cargo build
cargo test
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```

### Running Locally

```bash
# With a local kubeconfig
cargo run -- --kubeconfig ~/.kube/config

# Against a kind cluster
kind create cluster
cargo run
```

### CRD Development

CRDs are defined in `src/crd/`. After modifying CRD structs:

1. Regenerate CRD manifests (if applicable)
2. Update tests
3. Test against a real cluster with `kubectl apply`

## Architecture

- `src/crd/` — Custom Resource Definitions (StreamlineCluster, StreamlineTopic, StreamlineUser)
- `src/controllers/` — Reconciliation logic for each CRD
- `src/error.rs` — Error types

## License

By contributing, you agree that your contributions will be licensed under the Apache-2.0 License.
<!-- feat: 2f6e0c67 -->
