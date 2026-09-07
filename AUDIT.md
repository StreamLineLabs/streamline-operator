# Clean Code and SRP Audit

## Summary

- Controllers are already separated by Kubernetes resource actor; do not merge
  their similar finalizer/status/requeue shapes into a generic controller.
- **Highest-leverage future split:** isolate StatefulSet/Service/ConfigMap
  rendering from cluster reconciliation once golden manifest tests cover every
  option.
- `main.rs` now correctly owns startup and shutdown of controller/health/metrics
  tasks; further splitting would be small orchestration helpers, not new types.
- CRD modules are public serialized contracts and remain intact.
- Baseline formatting, strict Clippy, tests, integration gating, and auxiliary
  task shutdown are green.

## Findings

| ID | Location | Category | Severity | Actors in conflict | Cost | Size | Behavior risk |
|---|---|---|---|---|---|---|---|
| OP-SRP-1 | `controllers/cluster.rs` | Mixed reconciliation/rendering | P2 | cluster product; Kubernetes workload API | Desired-state construction and reconciliation/status orchestration change for separate actors. | L | High |
| OP-CC-1 | controller modules | Similar but actor-distinct code | P2 | cluster/topic/user/contract/branch/memory actors | Generic deduplication would couple independent CRD evolution and worsen SRP. | M | High |

## Ordered Refactor Sequence

1. Add golden tests for rendered cluster workloads across storage, auth, TLS,
   replicas, resources, and service settings.
2. Move rendering unchanged into `cluster_resources`.
3. Keep finalizer/requeue/status orchestration in `ClusterController`.
4. Do not create generic controller traits unless two CRDs share the same actor
   and lifecycle contract.

## Deferred

- Cluster renderer extraction lacks complete golden coverage.
- Live Kubernetes integration requires cluster credentials and a server image.
- CRD changes require versioned schema/release decisions.

## Out of Scope

- Per-CRD controllers: actor-aligned.
- CRD modules: serialization contracts.
- Metrics and leader election: independent operational actors.
