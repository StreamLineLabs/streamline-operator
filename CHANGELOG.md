# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).


## [Unreleased]

### ⚠️ Upgrading from 0.3.0 — action required before deploying

The 0.3.0 CRDs declared schema defaults this release rejects. Kubernetes applies
structural-schema defaults **on write**, so those values were persisted into
every stored object created against them — they are in etcd whether or not any
manifest mentions the fields, and installing the corrected CRDs does not rewrite
objects that already exist.

| Kind | Field | 0.3.0 persisted | Required now |
|------|-------|-----------------|--------------|
| `StreamlineCluster` | `spec.replicas` | `spec.replicas: 3` | `spec.replicas: 1` |
| `StreamlineCluster` | `spec.podAntiAffinity` | `spec.podAntiAffinity: true` | `spec.podAntiAffinity: false` |
| `StreamlineTopic` | `spec.replicationFactor` | `spec.replicationFactor: 2` | `spec.replicationFactor: 1` |
| `StreamlineTopic` | `spec.retention.retentionMs` | `spec.retention.retentionMs: 604800000` | `spec.retention.retentionMs: -1` |
| `StreamlineTopic` | `spec.config.minInsyncReplicas` | `spec.config.minInsyncReplicas: 1` | `spec.config.minInsyncReplicas: null` (remove the key) |
| `StreamlineTopic` | `spec.config.maxMessageBytes` | `spec.config.maxMessageBytes: 1048576` | `spec.config.maxMessageBytes: null` (remove the key) |

The three top-level fields were defaulted into every 0.3.0 resource. The three
nested ones reached etcd only for topics whose manifest opened a `retention:` or
`config:` block, because structural defaulting descends only into objects that
are present.

Patch every affected resource **before** deploying the new controller. The
commands are namespaced because the shipped operator watches one namespace,
while 0.3.0 ignored `--namespace` and watched them all — so affected resources
may live anywhere:

```bash
# Install the corrected CRDs first: 0.3.0 declares defaults for the two
# spec.config keys, so a merge-patch null is undone by defaulting on the same
# write until those CRDs are replaced.
kubectl apply -k deploy/crds/

kubectl patch streamlineclusters my-cluster -n streamline-system --type merge -p '{"spec":{"replicas":1}}'
kubectl patch streamlineclusters my-cluster -n streamline-system --type merge -p '{"spec":{"podAntiAffinity":false}}'
kubectl patch streamlinetopics events -n streamline-system --type merge -p '{"spec":{"replicationFactor":1}}'
kubectl patch streamlinetopics events -n streamline-system --type merge -p '{"spec":{"retention":{"retentionMs":-1}}}'
kubectl patch streamlinetopics events -n streamline-system --type merge -p '{"spec":{"config":{"minInsyncReplicas":null}}}'
kubectl patch streamlinetopics events -n streamline-system --type merge -p '{"spec":{"config":{"maxMessageBytes":null}}}'
```

Each patch is nested to the leaf so it corrects one setting and leaves its
siblings alone; `{"spec":{"config":null}}` would clear the same rejections while
discarding every other key in the block.

Two fields 0.3.0's hand-written CRDs advertised are removed from these schemas
and pruned by the API server on the next write:
`StreamlineCluster.spec.replication` (no 0.3.0 code path ever read it — the Rust
`ClusterSpec` had no such field) and `StreamlineTopic.spec.config.flushMs` (the
0.3.0 Rust type only ever had `flushIntervalMs`, so the stored value was
ignored). Both were inert, but their stored values are unrecoverable once
pruned — copy them out before installing the corrected CRDs.

Discovery, `--dry-run=server` rehearsal, bulk patch loops, verification, the
consequences for data on brokers `-1`/`-2`, and what a rollback does and does
not restore are in [docs/UPGRADING.md](docs/UPGRADING.md).

Resources left unpatched are **not** deleted, mutated, or silently ignored:
their existing StatefulSets, PVCs, and topics keep running, the resource reports
`phase: Failed` with `InvalidSpec` (cluster) or `UnsupportedConfiguration`
(topic), and the condition message names the field, the value to set, the exact
`kubectl patch`, and `docs/UPGRADING.md`. The operator deliberately does not
auto-mutate specs: rewriting `spec.replicas` from `3` to `1` would change a
durability decision on the user's behalf and drop the two brokers whose disks
hold unreplicated data.

### Added
- `docs/UPGRADING.md`, plus README and changelog release notes, documenting the
  incompatible 0.3.0 persisted defaults and the exact namespace-aware
  `kubectl patch --type merge` commands that clear them. Includes cluster-wide
  discovery commands, a server-side dry-run rehearsal, bulk patch loops,
  post-patch verification, and the rejection messages to expect for anything
  missed.
- `src/upgrade.rs`: one table of `(kind, field, 0.3.0 value, supported value,
  merge patch)` shared by the validation messages and the documentation tests,
  so a rejection and the upgrade guide cannot disagree about what to set.
- `tests/upgrade_from_v0_3_0.rs`: characterization tests that deserialize
  0.3.0-shaped custom resources, assert each is rejected for the right reason,
  and assert the documentation names every old value, new value, and patch
  command. Hermetic — no cluster or network.
- Deterministic CRD generator (`--generate-crds`, `--generate-crds-dir`,
  `make generate-crds`). `deploy/crds/*.yaml` is now generated from the
  `#[kube(...)]` annotations instead of hand-maintained, and `cargo test`, CI,
  and the release pipeline all fail if the two drift.
- `/readyz` reports `503` until the operator process is initialised
  (`src/health.rs`); it previously returned a hardcoded `ok`.
- `/leaderz` endpoint and a `streamline_operator_leader` gauge reporting which
  replica holds the leader Lease, so leadership can be observed without
  conflating it with readiness.
- `--namespace` is now implemented: a shared `WatchScope` resolves every enabled
  controller's `Api` to `Api::namespaced` or `Api::all`. The flag was previously
  parsed and logged but ignored — all controllers watched the whole cluster.
- `make release-manifests IMAGE=…` and `make verify-release TAG=… IMAGE=…`,
  the supported way to turn the checked-in manifests into a deployable one and
  to check tag/version/image agreement. Tagged releases publish the rendered
  `operator.yaml` (pinned to the pushed digest) as a release asset.
- Hermetic test suites `tests/crd_manifests.rs`, `tests/static_manifests.rs`,
  and `tests/docs_examples.rs` covering CRD drift, RBAC/kustomize/controller
  alignment, Dockerfile MSRV, probe paths and ordering, release image gating,
  namespace scope, and validation of every documented YAML example against the
  generated CRD schemas.
- `cargo audit` / `cargo deny` workflow and Dependabot coverage for the Docker
  ecosystem.
- `validate-cloud-fixture`, an offline binary used by the organization contract
  workflow to deserialize Cloud-generated CRs and run the real
  `ClusterSpec::validate()` and `TopicController::unsupported_fields()`
  acceptance paths.

### Removed
- `BranchController`, `ContractController`, and `MemoryController`, and the
  installation and RBAC for `StreamlineBranch`, `StreamlineContract`, and
  `StreamlineMemory`. The Streamline server exposes no compatible API for any of
  them: branches and agent memory sit behind non-default cargo features
  (`branches`, `agent-memory`) and take different request shapes, contracts are
  applied through `POST /api/v1/contracts/apply` with a different body (there is
  no `POST /api/v1/contracts`), and the memory controller only created ordinary
  topics before reporting the memory tiers `Ready`. Every reconcile failed,
  requeued, and failed again — a per-resource hot loop that also rewrote status
  each pass. The CRD types and generated schemas remain as schema-only
  references with the reason recorded in `Reconciliation::None(..)`; tests fail
  if one is re-enabled without changing that metadata.
- Secret `get`/`list`/`watch` from the operator's RBAC. The operator never calls
  the Secret API: TLS material is mounted through a `SecretVolumeSource` read by
  the kubelet, and no credentials are provisioned. The grant only widened the
  blast radius of a compromised operator. A static test fails if any shipped
  role mentions `secrets`.
- The `namespaces` RBAC rule: it is cluster-scoped, ungrantable by a Role, and
  unnecessary — the operator reads its own namespace from the projected service
  account token.

### Changed
- The opt-in cloud overlay labels `streamline-system` with
  `streamline.io/control-plane=true`, matching the namespace selector in
  Streamline Cloud's tenant NetworkPolicy so the cluster-wide topic controller
  can reach the private broker HTTP API on port 9094.
- Streamline CR RBAC is now limited to `get/list/watch/patch` on main resources,
  `patch` on status, and `update` on finalizers. The operator no longer receives
  create, replace, or delete authority over user-authored CRs, and static tests
  require the namespaced Role and cloud ClusterRole to remain identical.
- Rejections of the three 0.3.0 defaults now explain the upgrade instead of only
  refusing. `ClusterSpec::validate` (`spec.replicas`, `spec.podAntiAffinity`)
  and the topic gate (`spec.replicationFactor`) append the value to set, the
  namespaced `kubectl patch --type merge` that sets it, and a pointer to
  `docs/UPGRADING.md`; when the observed value *is* the 0.3.0 default they also
  say the API server persisted it and that the operator does not rewrite specs.
  A deliberately hand-written value (`replicas: 5`) gets the remediation without
  the upgrade provenance, because it did not come from one. The rejections
  themselves are unchanged — still fail-closed, still no auto-mutation.
- `deploy/operator.yaml` and `deploy/kustomization.yaml` no longer run a
  released operator image. The Deployment carries
  `ghcr.io/streamlinelabs/streamline-operator:REPLACE_WITH_RELEASED_IMAGE` with
  `imagePullPolicy: Always`, and the kustomize `images:`/`newTag` override was
  removed. The previous `:0.3.0` pin predates this tree, so applying the
  manifests silently ran an **older** operator than the CRDs and RBAC beside
  them; the default now fails closed at `ImagePullBackOff`. Static tests reject
  any runnable operator tag in the repository, and the release workflow
  substitutes the digest it just pushed after checking tag/version/label
  agreement.
- The shipped RBAC is a namespaced `Role`/`RoleBinding`
  (`deploy/rbac/role.yaml`, `role-binding.yaml`) instead of a `ClusterRole` and
  `ClusterRoleBinding`, and the Deployment passes
  `--namespace=$(OPERATOR_NAMESPACE)`. The default remains namespace-scoped;
  `overlays/cloud/` is the explicit cluster-wide mode, with reconciliation
  permissions in a least-privilege `ClusterRole` and leader-election Lease
  access kept in a namespaced `Role`.
- Topic settings the Streamline server does not apply are rejected before any
  API call. The topic API deserialises `POST /api/v1/topics` into
  `{ name, partitions }`, so the operator no longer sends retention, cleanup
  policy, compression, min-ISR, max message bytes, or segment bytes and then
  reports the topic `Ready`/`Synced`; any of those set to a non-default value
  now yields `phase: Failed` with an explanation. `Synced=True` requires the
  server to have echoed back both the partition count and the replication
  factor, and its message no longer implies any other configuration was applied.
- Stale operator-owned HPAs are removed on **every** cluster reconcile when
  autoscaling is absent, disabled, or invalid. Deleting the `spec.autoscaling`
  block previously orphaned the HPA it had created, which kept scaling the
  StatefulSet into independent brokers.
- `delete_hpa` propagates every Kubernetes error except `404`. It previously
  logged and swallowed all of them, so a `403` or `500` looked like a successful
  cleanup and the controller never retried.
- Broker pods are rendered from a **TOML** configuration mounted read-only at
  `/etc/streamline` and referenced by `STREAMLINE_CONFIG`; the previous YAML
  ConfigMap was never read by the server. The rendered file now uses the
  server's own sections and key names — `[server]` (`listen_addr`, `http_addr`,
  `data_dir`, `log_level`) and `[tls]` (`enabled`, `cert`, `key`,
  `require_client_cert`, `ca_cert`) — and no longer emits the top-level
  `raft_addr` and `metrics_enabled` keys, which the server's configuration file
  does not define.
- The default Streamline **server** image (`spec.image`, the integration
  harness, and the documented examples) moved from the stale
  `ghcr.io/streamlinelabs/streamline:0.2.0` to `:0.3.0`.
- TLS Secrets (and the mTLS CA bundle) are mounted into broker pods, so the
  certificate paths in the rendered configuration exist. They are mounted at
  `/etc/streamline-tls` and `/etc/streamline-tls-ca`, siblings of the read-only
  config mount rather than nested below it: a Secret volume mounted inside the
  ConfigMap mount is not a layout kubelet guarantees, and losing it would leave
  the broker pointed at certificate paths that do not exist. The pod receives
  `fsGroup: 1000` and the Secret files use mode `0440`, so the non-root broker
  can read them without making private keys world-readable. Unsupported TLS
  settings (`insecureSkipVerify`, `mtlsEnabled` without `caSecretName`, TLS
  without `secretName`) are rejected with an `InvalidSpec` status instead of
  being silently ignored.
- Readiness and leadership are separate state. The process is marked ready once
  the Kubernetes client and probe server exist, **before** it blocks on the
  leader Lease, so a standby replica passes `/readyz`. Gating readiness on
  leadership deadlocked rolling updates of an HA Deployment under the shipped
  `maxUnavailable: 1`: the new pod waited for the lease the outgoing pod still
  held, while the rollout waited for the new pod to become ready. Deployment
  readiness stays on `/readyz`; the active replica is identified by `/leaderz`.
- Enabling `spec.autoscaling` is rejected until clustered broker bootstrap is
  implemented; an HPA can no longer bypass the single-replica safety boundary.
- Unchanged `StreamlineUser` unsupported statuses are no longer patched on
  every watch event, preventing a status-write reconciliation loop.
- Broker readiness probes target `/health/ready` instead of the unserved
  `/ready`.
- `spec.replicas` defaults to `1` and `spec.image` defaults to the Streamline
  **server** image; the default previously pointed at the operator image and
  assumed clustering the operator does not bootstrap.
- `StreamlineTopic`: `replicationFactor` defaults to `1`, unsupported
  replication settings are rejected instead of reported `Ready`, and status is
  derived from the server's response (with `Synced=Unknown` when the server
  reports nothing).
- `StreamlineUser` is reported as `Unsupported`: the operator no longer calls
  the non-existent `/api/v1/users` endpoint and no longer provisions a
  credentials Secret the cluster never learns about.
- `StreamlineContract` short name changed from `slc` (which collided with
  `StreamlineCluster`) to `slcon`.
- Operator RBAC now matches the installed CRDs exactly and holds no Secret
  access.
- Docker builder image raised from `rust:1.75` to `rust:1.88`, matching the
  declared MSRV.
- CodeQL no longer analyses this Rust repository as C++.

### Fixed
- `StreamlineCluster` is `Ready` and non-degraded only when the number of ready
  broker pods exactly matches `spec.replicas`. Zero, partial, and excess ready
  counts now report an unhealthy/degraded state instead of using the `Healthy`
  reason.
- Release CRD generation fails closed instead of skipping with a message.
- TLS and mTLS CA Secrets are no longer mounted *inside* the read-only ConfigMap
  mount. They now use the sibling paths `/etc/streamline-tls` and
  `/etc/streamline-tls-ca`; a Secret volume nested under another volume's mount
  point is not a layout kubelet guarantees, and losing it left the broker with a
  config file claiming TLS while `cert`/`key` pointed at missing paths.
- `StreamlineCluster` and `StreamlineTopic` status updates no longer self-trigger
  an unbounded reconcile loop. Both controllers now follow the pattern already
  used by the user controller: conditions are seeded from the currently published
  status so unchanged ones keep their `lastTransitionTime`, `lastUpdated` is only
  restamped when the rest of the status actually changed, and the patch is
  skipped entirely when the desired status matches what is already stored.
  Previously every reconcile rebuilt all timestamps, so each status patch
  produced a watch event that immediately re-entered reconciliation.
- Rejecting `autoscaling.enabled` now also removes an HPA left behind by an
  earlier version of the operator. The rejection previously only declined to
  create a *new* HPA, so an existing one survived the upgrade and kept scaling
  the StatefulSet into multiple independent standalone brokers — the split-brain
  the rejection exists to prevent. Cleanup runs before the validation early
  return; the single-replica and autoscaling rejections themselves are unchanged.
- Every documented custom resource example now sets
  `metadata.namespace: streamline-system`, the namespace the shipped Deployment
  actually watches. The quick start in `README.md`, `docs/API.md`, and the crate
  docs created resources in `default` (or in whatever namespace `kubectl`
  defaulted to), while `deploy/operator.yaml` passes
  `--namespace=$(OPERATOR_NAMESPACE)` and `deploy/rbac/` grants a namespaced
  `Role` in `streamline-system`. Following the documentation therefore produced
  a resource the API server accepted and the operator never saw: no status, no
  events, no error. `tests/docs_examples.rs` now derives the watched namespace
  from `deploy/operator.yaml` (resolving `$(OPERATOR_NAMESPACE)` through the
  downward-API env binding to the Deployment's namespace), cross-checks it
  against the shipped `Role`, `RoleBinding`, `ServiceAccount`, and
  `namespace.yaml`, and fails on any documented example whose namespace differs
  or is missing.
- `spec.retention.retentionMs` now defaults to `-1` (unlimited) instead of
  `604800000` (7 days), matching `retentionBytes` and what the broker actually
  does. The core topic API accepts only `{name, partitions}` and applies no
  topic configuration, so nothing ever enforced a seven-day window — but the
  CRD default, `kubectl explain`, and every server-defaulted resource announced
  one. Because the controller rejects non-default values (unchanged: retention,
  compression, cleanup policy, and `config` overrides are still fail-closed),
  the wrong value was also the only one users were allowed to keep. A
  characterization test in `src/controllers/topic.rs` pins that a default
  `TopicSpec` is unlimited and publishes no seven-day claim, and that an
  explicit `retentionMs: 604800000` is still rejected.
- `README.md` and `docs/API.md` documented topic settings under field paths and
  shapes the CRD does not have — `spec.retention.ms`, `spec.retention.bytes`,
  and a scalar `spec.compression`. They now use the real
  `spec.retention.retentionMs`, `spec.retention.retentionBytes`,
  `spec.retention.cleanupPolicy`, and `spec.compression.type`, record the
  corrected defaults, and no longer claim a seven-day retention policy.
- `spec.env[].valueFrom` is applied instead of dropped. The field is in the
  shipped schema, so the API server accepted it, and the cluster controller then
  rendered only `name`/`value` — a variable declared as a `secretKeyRef` reached
  the broker as the empty string, which is worse than a failure for a password
  or token. `secretKeyRef` and `configMapKeyRef` are now mapped verbatim onto
  the container's `EnvVarSource`, so the kubelet resolves the reference at pod
  start and the operator still needs no `secrets` RBAC. Shapes that cannot be
  mapped exactly — `value` together with `valueFrom`, both references at once,
  an empty `valueFrom`, a blank `name`/`key`, or an entry with neither `value`
  nor `valueFrom` (write `value: ""` for a deliberately empty one) — are
  rejected by `ClusterSpec::validate`, and the renderer independently refuses to
  emit a variable it cannot source.
- `spec.podAntiAffinity`, `spec.rackAwareness`, and `spec.tolerations` are
  rejected rather than advertised and ignored. The pod template renders no
  affinity, topology spread, or toleration, so a cluster asking to be spread
  across nodes was accepted, reported `Ready`, and scheduled wherever the
  scheduler happened to put it. `podAntiAffinity` now defaults to `false` (it
  claimed `true`), enabling any of the three fails with `InvalidSpec`, the
  schema descriptions and the README/API tables mark them as not applied, and
  the quick start no longer sets `podAntiAffinity: true`. `spec.nodeSelector` is
  unaffected — it is rendered, and remains the supported placement control.
- `docs/ENVIRONMENT.md` listed `--generate-crds` and `--generate-crds-dir` as
  table rows placed *after* two explanatory paragraphs, so Markdown ended the
  flag table before them and both rendered as literal pipe-delimited text. The
  rows are back inside the table, and `tests/static_manifests.rs` now fails on
  any documentation table row separated from its table by prose, plus checks
  that every operator flag appears in the flag table itself.


## [0.3.0] - 2026-04-20

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
