# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).


## [Unreleased]

### Added (Moonshot scaffolds)
- **`StreamlineBranch` CRD** (`streamline.io/v1alpha1`, kind `StreamlineBranch`, short name `slb`):
  declarative time-travel branches (Moonshot M5). Spec: `clusterRef`, `parent`,
  `description`, `retention.ttlSeconds`. Status: `phase` (Pending/Creating/Ready/Failed/Deleting),
  `ready`, `createdAtMs`, `message`, `conditions`. Source `src/crd/branch.rs` + manifest
  `deploy/crds/streamlinebranch-crd.yaml`.
- **`StreamlineContract` CRD** (`streamline.io/v1alpha1`, kind `StreamlineContract`,
  short name `slc`): declarative enforced data contracts (Moonshot M4). Spec:
  `clusterRef`, `schemaJson`, `compatibility` (BACKWARD/FORWARD/FULL/NONE; default
  BACKWARD), `bindTopics`. Status: `phase`, `registered`, `boundTopics`, `message`,
  `conditions`. Source `src/crd/contract.rs` + manifest `deploy/crds/streamlinecontract-crd.yaml`.
- Both CRDs are registered in `deploy/crds/kustomization.yaml`.
- 10 new unit tests covering defaults, round-trip serialization, enum encoding,
  and CRD generation via `kube::CustomResourceExt::crd()`.
- **No live controllers yet**: these CRDs are declarative scaffolds for GitOps
  flows. Reconcilers that drive the Moonshot HTTP control plane will land in a
  follow-up. `cargo test --lib` now passes 88/88 (was 78/78).

- test: add envtest suite for topic reconciler
- test: add envtest for cluster rolling upgrade (2026-03-05)
- refactor: extract CRD validation into shared module (2026-03-06)
- fix: resolve reconciliation loop on status update (2026-03-06)
- **Changed**: update kube-rs to latest version
- **Changed**: extract status update logic into trait
- **Fixed**: resolve pod restart loop on config change
- **Added**: add StreamlineTopic CRD reconciliation
- **Changed**: update kube-rs to latest version
- **Testing**: add controller unit tests with mock API
- **Changed**: extract status update logic into trait
- **Fixed**: resolve pod restart loop on config change
- **Added**: add StreamlineTopic CRD reconciliation

### Fixed
- Handle CRD status update race condition

### Changed
- Update kube-rs dependency
- Extract reconciler into separate module
- Simplify operator state machine transitions


## [0.2.0] - 2026-02-18

### Added
- `StreamlineCluster` CRD for managing Streamline cluster lifecycle
- `StreamlineTopic` CRD for declarative topic management
- `StreamlineUser` CRD for user and access control management
- Full reconciliation pipeline with status reporting
- Rust-based operator using kube-rs framework
- CI pipeline with formatting, linting, and tests
- fix: handle missing CRD annotations in reconcile loop
- docs: document topic reconciliation state machine flow
- chore: bump kube-rs dependency to 0.89
- feat: implement horizontal autoscaling for StreamlineCluster
- chore: update CRD schema and generated documentation
