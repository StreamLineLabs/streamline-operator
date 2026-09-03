# Operator API Reference

> **Upgrading from v0.3.0?** Its CRDs persisted `spec.replicas: 3`,
> `spec.podAntiAffinity: true`, and `spec.replicationFactor: 2` into existing
> objects, and this operator rejects all three without mutating your specs.
> Patch them first — see [UPGRADING.md](UPGRADING.md).

## Custom Resource Definitions

### StreamlineCluster

```yaml
apiVersion: streamline.io/v1alpha1
kind: StreamlineCluster
metadata:
  name: my-cluster
spec:
  # Each replica is a standalone broker: the operator does not bootstrap raft
  # peers, so the default (and only verified) value is 1.
  replicas: 1
  storage:
    size: 10Gi
    storageClassName: standard
  resources:
    requests:
      cpu: 500m
      memory: 1Gi
    limits:
      cpu: "2"
      memory: 4Gi
```

`spec.autoscaling.enabled: true` is currently rejected with `InvalidSpec`.
Scaling the standalone StatefulSet would create independent brokers rather than
a quorum and would make the HPA compete with the controller for
`spec.replicas`.

#### Environment variables

`spec.env` accepts a literal `value` or a `valueFrom` reference, and the
reference is rendered straight onto the broker container so the kubelet
resolves it at pod start — the operator never reads the Secret or ConfigMap and
needs no RBAC for either:

```yaml
apiVersion: streamline.io/v1alpha1
kind: StreamlineCluster
metadata:
  name: my-cluster
  namespace: streamline-system
spec:
  env:
    - name: STREAMLINE_EXTRA
      value: "on"
    - name: SASL_PASSWORD
      valueFrom:
        secretKeyRef:
          name: broker-auth
          key: password
    - name: STREAMLINE_TUNING
      valueFrom:
        configMapKeyRef:
          name: broker-tuning
          key: flags
```

`secretKeyRef` and `configMapKeyRef` are the two supported references. Setting
both `value` and `valueFrom`, setting both references, leaving `valueFrom`
empty, leaving a reference's `name`/`key` blank, or declaring an entry with
neither `value` nor `valueFrom` is rejected with `InvalidSpec` rather than
rendered as an empty variable. Write `value: ""` to set one deliberately.

#### Scheduling

`spec.nodeSelector` is applied. `spec.podAntiAffinity`, `spec.rackAwareness`,
and `spec.tolerations` are **not**: the pod template carries no affinity,
topology spread, or toleration, so enabling anti-affinity or rack awareness, or
declaring any toleration, is rejected with `InvalidSpec` instead of being
accepted and ignored.

### StreamlineTopic

```yaml
apiVersion: streamline.io/v1alpha1
kind: StreamlineTopic
metadata:
  name: events
spec:
  clusterRef: my-cluster
  partitions: 6
  # Values other than 1 are rejected: the server creates single-replica topics.
  replicationFactor: 1
```

Only `partitions` is applied. The Streamline topic API deserialises
`POST /api/v1/topics` into `{ name, partitions }`, hard-codes the replication
factor to `1`, and offers no way to set topic configuration or to update an
existing topic. `spec.retention`, `spec.compression`, and `spec.config`
therefore exist in the schema but are **rejected** whenever they differ from
their defaults, instead of being sent and silently discarded:

```text
status.phase: Failed
status.conditions[Ready].reason: UnsupportedConfiguration
status.conditions[Synced].status: False
```

`Synced=True` is claimed only when the server echoed back both the partition
count and the replication factor and both match the spec. `GET
/api/v1/topics/{name}` also returns a `config` map, but it reports the
*server's* settings (`retention.ms: -1`, `segment.bytes: 104857600`, …) rather
than anything the operator requested, so it is deliberately not used to
establish agreement.

#### Topic settings that exist in the schema but are never applied

| Field | Type | Default | Notes |
|-------|------|---------|-------|
| `spec.retention.retentionMs` | int64 | `-1` | Unlimited; the broker never expires a segment on age |
| `spec.retention.retentionBytes` | int64 | `-1` | Unlimited |
| `spec.retention.cleanupPolicy` | string | `delete` | No compaction is performed |
| `spec.compression.type` | string | `producer` | Compression is chosen by the producer |
| `spec.config.*` | object | unset | Every entry is an override the server drops |

The defaults are the *accepted* values: setting any of them to something else
fails the resource with `UnsupportedConfiguration` rather than sending a request
the server discards.

`retentionMs` defaulted to `604800000` (7 days) until it was corrected to `-1`.
That default described a retention policy the broker does not implement, and
since every non-default value is rejected, it was simultaneously the only
permitted value and a false one. Both retention axes now agree with each other
and with what the server does.

### StreamlineUser

> ⚠️ **Unsupported.** The Streamline server exposes no user API. The controller
> publishes `status.phase: Unsupported` with a `Ready=False`
> (`reason: UnsupportedByServer`) condition and creates nothing — no user, no
> ACLs, no quotas, and no credentials Secret.

```yaml
apiVersion: streamline.io/v1alpha1
kind: StreamlineUser
metadata:
  name: app-producer
  namespace: streamline-system
spec:
  clusterRef: my-cluster
  authentication:
    type: scram-sha512
```

### Schema-only kinds

`StreamlineBranch`, `StreamlineContract`, `StreamlineMemory`,
`StreamlineBackup`, and `StreamlineEdge` are generated into `deploy/crds/` for
reference but are **not installed**, have no controller, and hold no RBAC. Each
generated manifest states why in its header. See the README's "Schema-only
CRDs" section.

## CRD manifests

`deploy/crds/*.yaml` is generated from the Rust types:

```bash
make generate-crds                      # rewrite deploy/crds/
streamline-operator --generate-crds     # print the installed manifests
```

`cargo test` (and CI, and the release pipeline) fail if the checked-in
manifests drift from the generator, and `tests/docs_examples.rs` validates every
YAML example on this page against the generated schemas.

## Deploying the operator

`deploy/operator.yaml` carries an intentionally unpullable placeholder image so
the checked-in manifests can never run an operator older than the repository.
Render a deployable manifest from an explicit, immutable reference:

```bash
make release-manifests IMAGE=ghcr.io/streamlinelabs/streamline-operator@sha256:<digest>
kubectl apply -f deploy/operator.release.yaml
```

Tagged releases publish this rendered `operator.yaml` as a release asset.
