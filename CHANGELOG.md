# Changelog

All notable changes to this project will be documented in this file.
- test: add unit tests for topic controller (2026-02-22)
- refactor: extract reconciler into separate module (2026-02-22)
- fix: handle CRD status update race condition (2026-02-22)
- refactor: simplify operator state machine transitions (2026-02-21)

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-02-18

### Added
- `StreamlineCluster` CRD for managing Streamline cluster lifecycle
- `StreamlineTopic` CRD for declarative topic management
- `StreamlineUser` CRD for user and access control management
- Full reconciliation pipeline with status reporting
- Rust-based operator using kube-rs framework
- CI pipeline with formatting, linting, and tests
