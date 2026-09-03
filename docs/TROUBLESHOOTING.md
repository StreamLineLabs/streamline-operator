# Operator Troubleshooting

Every command here is namespace-scoped. The shipped `deploy/operator.yaml`
passes `--namespace=$(OPERATOR_NAMESPACE)`, so the operator only reconciles
resources in its own namespace — `streamline-system` unless you changed it.
Substitute your own namespace and cluster name throughout.

## Labels and selectors

The operator labels everything it creates with `app.kubernetes.io/*`. There is
no `app` label on anything, so a selector of `app=streamline` matches nothing —
which is indistinguishable from a resource that was never created.

| Selector | Matches |
|----------|---------|
| `app.kubernetes.io/name=streamline-operator` | The operator Deployment and its pod |
| `app.kubernetes.io/name=streamline` | Broker pods, Services, and ConfigMaps for **every** cluster |
| `app.kubernetes.io/instance=<cluster-name>` | Everything belonging to one `StreamlineCluster` |
| `app.kubernetes.io/managed-by=streamline-operator` | Everything the operator owns |

Select one cluster by combining name and instance — `instance` alone is not
unique across kinds, and `name` alone spans every cluster in the namespace:

```bash
kubectl get pods,svc,cm -n streamline-system \
  -l app.kubernetes.io/name=streamline,app.kubernetes.io/instance=my-cluster
```

## Upgrading from v0.3.0

### `Failed` / `InvalidSpec` immediately after upgrading

The v0.3.0 CRDs persisted six values into stored objects that this operator
rejects. It does not rewrite specs, so those resources report `phase: Failed`
until they are patched:

```bash
# Install the corrected CRDs first: v0.3.0 defaults the two spec.config keys, so
# a merge-patch null is undone by defaulting on the same write until it is gone.
kubectl apply -k deploy/crds/

kubectl patch streamlineclusters my-cluster -n streamline-system --type merge -p '{"spec":{"replicas":1}}'
kubectl patch streamlineclusters my-cluster -n streamline-system --type merge -p '{"spec":{"podAntiAffinity":false}}'
kubectl patch streamlinetopics events -n streamline-system --type merge -p '{"spec":{"replicationFactor":1}}'
kubectl patch streamlinetopics events -n streamline-system --type merge -p '{"spec":{"retention":{"retentionMs":-1}}}'
kubectl patch streamlinetopics events -n streamline-system --type merge -p '{"spec":{"config":{"minInsyncReplicas":null}}}'
kubectl patch streamlinetopics events -n streamline-system --type merge -p '{"spec":{"config":{"maxMessageBytes":null}}}'
```

The condition message names the field, the value to write, the patch, and
`docs/UPGRADING.md`. Read it before guessing:

```bash
kubectl get streamlinetopic events -n streamline-system \
  -o jsonpath='{.status.conditions[?(@.type=="Ready")].message}{"\n"}'
```

Full procedure, including cluster-wide discovery, the CRD-install ordering, and
verification: [UPGRADING.md](UPGRADING.md).

## CRD Issues

### StreamlineCluster not reconciling

```bash
# Operator logs (the operator is labelled by name, not `app`)
kubectl logs -n streamline-system -l app.kubernetes.io/name=streamline-operator --tail=200

# CRDs installed?
kubectl get crds | grep streamline.io

# What the operator decided about this resource
kubectl get streamlinecluster my-cluster -n streamline-system -o jsonpath='{.status}{"\n"}'
```

Check in this order:

1. **Is the resource in the namespace the operator watches?** The shipped
   deployment is namespace-scoped. A `StreamlineCluster` anywhere else is never
   seen, and nothing is logged about it — there is no error, just silence.
2. **Is this replica the leader?** With more than one operator replica, only the
   leader reconciles. `/readyz` is true on a standby; leadership is `/leaderz`
   and the `streamline_operator_leader` gauge.
3. **Was the spec rejected?** `status.phase: Failed` with reason `InvalidSpec`
   means the operator read the resource and refused it. The condition message
   says which field.
4. **RBAC.** The shipped `deploy/rbac/` is a namespaced `Role`/`RoleBinding`.
   Cluster-wide mode (`--namespace=`) must be installed from
   `overlays/cloud/`; using the default RBAC with an empty watch flag makes
   every cross-namespace list/watch fail with `Forbidden`.

### Spec fields the operator rejects on purpose

These are not bugs and no configuration enables them. Each is refused because
nothing renders it, and reporting `Ready` for a setting that was dropped would
be worse than failing:

| Field | Why it is rejected |
|-------|--------------------|
| `spec.replicas` > 1 | Nothing bootstraps raft peers, so N replicas are N independent brokers |
| `spec.podAntiAffinity: true` | The pod template renders no `affinity` |
| `spec.rackAwareness.enabled: true` | Nothing renders topology spread constraints |
| `spec.tolerations` (non-empty) | The pod template renders no tolerations; `spec.nodeSelector` is the only scheduling control that reaches the pod |
| `spec.autoscaling.enabled: true` | See [Autoscaling](#autoscaling) below |
| `StreamlineTopic.spec.replicationFactor` != 1 | The topic API creates single-replica topics |
| Any non-default `StreamlineTopic.spec.retention` / `spec.compression`, or any `spec.config` entry | The topic API accepts only `{name, partitions}` and applies no topic configuration |

`StreamlineUser` is reported `Unsupported` and creates nothing: the Streamline
server has no user API.

### Pod not starting

```bash
kubectl get pods -n streamline-system -l app.kubernetes.io/instance=my-cluster
kubectl describe pod my-cluster-0 -n streamline-system
kubectl get pvc -n streamline-system -l app.kubernetes.io/instance=my-cluster
```

- `ImagePullBackOff` on the **operator** is expected from the checked-in
  manifest: `deploy/operator.yaml` ships
  `ghcr.io/streamlinelabs/streamline-operator:REPLACE_WITH_RELEASED_IMAGE` with
  `imagePullPolicy: Always` so it can never silently run an older operator.
  Render a deployable manifest with
  `make release-manifests IMAGE=<repo>@sha256:<digest>`.
- `Pending` with no events usually means the PVC is unbound — check that a
  default StorageClass exists.
- Check resource requests against what the nodes can offer.

## Autoscaling

**Autoscaling is rejected, not tuned.** `spec.autoscaling.enabled: true` fails
validation with `InvalidSpec` because the operator renders a single standalone
broker and cannot bootstrap raft peers; an HPA would create additional
independent brokers that share a Service name and hold unreplicated partitions.
There is no metrics-server requirement, no threshold to adjust, and no
`spec.autoscaling` field that makes it work.

```text
Status:
  Phase:  Failed
  Conditions:
    Type:     Ready
    Status:   False
    Reason:   InvalidSpec
    Message:  autoscaling.enabled is not supported: the operator only renders a
              standalone broker and cannot safely scale it above one replica
```

Every cluster reconcile also **deletes** any operator-owned
`<cluster-name>-hpa` when autoscaling is absent, disabled, or invalid — an HPA
left behind by an earlier version would otherwise keep scaling a StatefulSet
nobody is reconciling. If an HPA reappears, something other than this operator
is creating it:

```bash
kubectl get hpa -n streamline-system
kubectl get hpa my-cluster-hpa -n streamline-system -o jsonpath='{.metadata.ownerReferences}{"\n"}'
```

To clear the rejection, remove the block (or set `enabled: false`) and keep
`spec.replicas: 1`:

```bash
kubectl patch streamlineclusters my-cluster -n streamline-system --type merge \
  -p '{"spec":{"autoscaling":null}}'
```

## Network Issues

### Clients cannot connect to cluster

The operator creates two Services per cluster: `<cluster-name>` (client-facing,
ports `kafka` and `http`) and `<cluster-name>-headless` (per-pod DNS, adding
`raft`). Both carry the `app.kubernetes.io/*` labels; the pod selector is
`app.kubernetes.io/name=streamline` plus `app.kubernetes.io/instance=<cluster>`.

```bash
# Services for one cluster
kubectl get svc -n streamline-system \
  -l app.kubernetes.io/name=streamline,app.kubernetes.io/instance=my-cluster

# Do the endpoints actually resolve to a running pod?
kubectl get endpoints my-cluster -n streamline-system

# Reach the broker without leaving the cluster network
kubectl port-forward -n streamline-system svc/my-cluster 9092:9092
```

- Empty endpoints mean no pod matches the Service selector, or the pod is not
  `Ready` — check the broker's readiness probe before blaming the network.
- If a NetworkPolicy is in use, allow the Kafka port (`spec.kafkaPort`,
  default 9092) and the HTTP port (`spec.httpPort`, default 9094).
- For external access, verify the LoadBalancer or Ingress in front of
  `<cluster-name>`; the operator does not create either.
