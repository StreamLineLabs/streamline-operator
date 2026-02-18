# Streamline Operator

[![CI](https://github.com/streamlinelabs/streamline-operator/actions/workflows/ci.yml/badge.svg)](https://github.com/streamlinelabs/streamline-operator/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)
[![Rust](https://img.shields.io/badge/Rust-1.75+-DEA584.svg)](https://www.rust-lang.org/)
[![Kubernetes](https://img.shields.io/badge/Kubernetes-1.26+-326CE5.svg)](https://kubernetes.io/)

Kubernetes operator for managing Streamline clusters, topics, and users.

## Prerequisites

- Rust 1.75+
- Access to a Kubernetes cluster (kind/minikube/GKE/etc.)
- `kubectl` configured with a valid kubeconfig

## Build

```bash
cargo build -p streamline-operator
```

## Run Locally

```bash
cargo run -p streamline-operator -- --help
cargo run -p streamline-operator -- --namespace default
```

## Project Layout

- `src/controllers/` — reconciliation logic for cluster, topic, and users
- `src/crd/` — CRD definitions
- `src/main.rs` — operator entrypoint
