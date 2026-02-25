# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]
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
