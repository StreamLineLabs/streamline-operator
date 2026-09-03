# Upgrading from v0.3.0

> **Read this before you deploy an operator built from this tree over a v0.3.0
> installation.** v0.3.0 wrote six values into your stored custom resources
> that this operator rejects. Nothing is auto-corrected, so every resource that
> still carries them stops reconciling until you patch it. The patches are ten
> minutes of `kubectl`; skipping them looks like the operator silently
> abandoning your clusters and topics.

- [What changed](#what-changed)
- [Fields v0.3.0 had that this tree does not](#fields-v030-had-that-this-tree-does-not)
- [Why your resources carry values you never wrote](#why-your-resources-carry-values-you-never-wrote)
- [What happens if you skip this](#what-happens-if-you-skip-this)
- [Why the operator does not fix it for you](#why-the-operator-does-not-fix-it-for-you)
- [Before you patch: brokers 1 and 2 hold unreplicated data](#before-you-patch-brokers-1-and-2-hold-unreplicated-data)
- [Step 1 — Find every affected resource](#step-1--find-every-affected-resource)
- [Step 2 — Install the corrected CRDs first](#step-2--install-the-corrected-crds-first)
- [Step 3 — Dry-run the patches](#step-3--dry-run-the-patches)
- [Step 4 — Patch the stored objects](#step-4--patch-the-stored-objects)
- [Step 5 — Verify before deploying](#step-5--verify-before-deploying)
- [Step 6 — Deploy the controller](#step-6--deploy-the-controller)
- [What a missed resource looks like](#what-a-missed-resource-looks-like)
- [Rolling back](#rolling-back)

## What changed

Six fields whose v0.3.0 **schema defaults** are refused by this operator:

| Kind | Field | v0.3.0 persisted | This operator requires | Why the old value is refused |
|------|-------|------------------|------------------------|------------------------------|
| `StreamlineCluster` | `spec.replicas` | `spec.replicas: 3` | `spec.replicas: 1` | Nothing bootstraps raft peers, so three replicas render three **independent** brokers that look like a quorum and are not one |
| `StreamlineCluster` | `spec.podAntiAffinity` | `spec.podAntiAffinity: true` | `spec.podAntiAffinity: false` | The pod template renders no `affinity` at all, so the spread was advertised and never applied |
| `StreamlineTopic` | `spec.replicationFactor` | `spec.replicationFactor: 2` | `spec.replicationFactor: 1` | The Streamline topic API creates single-replica topics; a factor of 2 claims durability the cluster does not provide |
| `StreamlineTopic` | `spec.retention.retentionMs` | `spec.retention.retentionMs: 604800000` | `spec.retention.retentionMs: -1` | The topic API accepts only `{name, partitions}` and applies no topic configuration, so nothing ever enforced the seven-day window. `-1` (unlimited) is what the broker actually does |
| `StreamlineTopic` | `spec.config.minInsyncReplicas` | `spec.config.minInsyncReplicas: 1` | `spec.config.minInsyncReplicas: null` (remove the key) | Same reason, and there is no value to move it to: every `spec.config` entry is an override the server discards, so the field has to go away rather than change |
| `StreamlineTopic` | `spec.config.maxMessageBytes` | `spec.config.maxMessageBytes: 1048576` | `spec.config.maxMessageBytes: null` (remove the key) | Same: a message-size limit the broker never received, so it was advertised and unenforced |

The first four are corrected by **setting a value**; the last two by **removing
the key**, which is what a JSON merge-patch `null` does. Removal has an ordering
requirement the others do not — see [Step 2](#step-2--install-the-corrected-crds-first).

The same table drives the operator's own error messages and the tests in
`tests/upgrade_from_v0_3_0.rs`; it lives in `src/upgrade.rs`.

## Fields v0.3.0 had that this tree does not

The API group and version are unchanged (`streamline.io/v1alpha1`) and every
field listed above still exists under the same name. Two fields do **not**:
`StreamlineCluster.spec.replication` and `StreamlineTopic.spec.config.flushMs`.

| Kind | Field v0.3.0 advertised | Status now |
|------|-------------------------|------------|
| `StreamlineCluster` | `spec.replication` (`enabled`, `mode`, `remotes`, `topicFilter`, `maxLagMs`) | **Removed from the schema.** v0.3.0's hand-written CRD accepted this block, but v0.3.0's `ClusterSpec` had no such field, so nothing ever read it. There is no geo-replication in the operator and no replacement field |
| `StreamlineTopic` | `spec.config.flushMs` | **Removed from the schema.** v0.3.0's CRD advertised `flushMs` while its Rust type only had `flushIntervalMs`, so the value was stored and ignored. `spec.config.flushIntervalMs` is the field that exists today — and it is itself rejected, because the topic API applies no configuration |

Both were inert in v0.3.0, so removing them changes no behaviour. What it
changes is what the API server keeps:

- **Structural schemas prune unknown fields.** Once the corrected CRDs are
  installed, `spec.replication` and `spec.config.flushMs` are no longer part of
  the schema, so the API server drops them from the object on the next write —
  including the `kubectl patch` commands in this guide. `kubectl get -o yaml`
  stops showing them.
- **The values are not recoverable from the cluster afterwards.** If you have a
  `spec.replication` block whose contents you still need — remote bootstrap
  addresses, TLS secret names — copy it out **before** Step 2:

  ```bash
  kubectl get streamlineclusters --all-namespaces -o json |
    jq '[.items[] | select(.spec.replication != null)
         | {namespace: .metadata.namespace, name: .metadata.name, replication: .spec.replication}]' \
    > streamline-replication-backup.json

  kubectl get streamlinetopics --all-namespaces -o json |
    jq '[.items[] | select(.spec.config.flushMs != null)
         | {namespace: .metadata.namespace, name: .metadata.name, flushMs: .spec.config.flushMs}]' \
    > streamline-flushms-backup.json
  ```

- **Re-applying an old manifest is now an error, not a silent drop.** `kubectl
  apply` warns about unknown fields (`--validate=strict` rejects them), so remove
  `replication:` and `flushMs:` from the manifests in your Git repository too.

Neither field blocks the upgrade: the operator does not report them as
unsupported, because it cannot see them.


## Why your resources carry values you never wrote

Kubernetes applies CRD structural-schema defaults **on write**, and persists the
result. Every `StreamlineCluster` and `StreamlineTopic` created or updated while
the v0.3.0 CRDs were installed therefore has these values *stored in etcd*, even
if your manifest never mentioned the fields and your Git repository still does
not:

```text
$ kubectl get streamlinecluster my-cluster -n streamline-system -o yaml
spec:
  replicas: 3            # you did not write this; the v0.3.0 CRD default did
  podAntiAffinity: true  # nor this
  ...
```

**Top-level fields were defaulted everywhere; nested ones were not.** Defaulting
only descends into objects that are actually present in the submitted document,
and v0.3.0 gave neither `spec.retention` nor `spec.config` a default of its own.
So:

- `spec.replicas`, `spec.podAntiAffinity`, and `spec.replicationFactor` are in
  **every** resource created against v0.3.0.
- `spec.retention.retentionMs`, `spec.config.minInsyncReplicas`, and
  `spec.config.maxMessageBytes` are in every topic whose manifest **opened the
  surrounding block** — including an empty `config: {}` or a `retention:` block
  that only set `cleanupPolicy` — and in no other. A topic that never mentioned
  either block carries neither.

That is why the discovery queries in Step 1 are split, and why one of them can
legitimately print nothing while another prints a long list.

Three more consequences trip people up:

1. **Installing the corrected CRDs does not fix stored objects.** New defaults
   apply to future writes. The object keeps `3` / `true` / `2` / `604800000`
   until something writes a new value — which is what the patches below do.
2. **Deleting the field from your manifest does not fix it either.** Under a
   `kubectl apply`, an omitted field is re-defaulted by whichever CRD is
   installed at that moment. For the four value fields, set the value
   explicitly. For the two `spec.config` keys there is no value to set, so the
   removal has to happen *after* the corrected CRDs are installed — Step 2.
3. **Reads are defaulted too.** Once the corrected CRDs are installed, a topic
   that never stored `spec.retention` reads back as `retentionMs: -1`, because
   the API server applies the storage-version defaults on read. A topic that
   stored `604800000` still reads `604800000`. The Step 1 queries are correct
   either side of the CRD install.

v0.3.0 also ignored `--namespace` and watched **every** namespace, so affected
resources may exist outside `streamline-system`. The discovery commands below
are cluster-wide for exactly that reason; the patches are namespace-scoped
because a patch aimed at the wrong namespace succeeds against nothing and
reports no error.

## What happens if you skip this

The corrected operator fails closed. It does **not** delete anything, and your
existing StatefulSets, PVCs, Services, and topics keep running exactly as they
are — but reconciliation stops on the affected resources:

| Resource | Status after upgrade | Effect |
|----------|----------------------|--------|
| `StreamlineCluster` with `spec.replicas: 3` or `spec.podAntiAffinity: true` | `phase: Failed`, `Ready=False` with reason `InvalidSpec` | No ConfigMap, Service, or StatefulSet changes are applied; requeued every 300s |
| `StreamlineTopic` with `spec.replicationFactor: 2`, a non-default `spec.retention`, or any `spec.config` entry | `phase: Failed`, `Ready=False` with reason `UnsupportedConfiguration` | The topic is not created or reconciled; requeued every 300s |

That is deliberate: the alternative is running three brokers that share a
service name, hold unreplicated partitions, and report `Ready` as though they
were a cluster — or a topic whose status claims a seven-day retention and a
1 MiB message cap that no broker was ever told about.


## Why the operator does not fix it for you

Auto-mutating `spec.replicas` from `3` to `1` would let a controller change a
durability decision on your behalf, in a field you can read, without a record of
who changed it or when — and it would remove exactly the two brokers whose disks
hold data no other broker has (see the next section). Silently rewriting user
intent is how the v0.3.0 defaults became invisible in the first place.

So the operator refuses, and every rejection names the field, the value to set,
the patch that sets it, and this document.

## Before you patch: brokers 1 and 2 hold unreplicated data

If a cluster really was running with `spec.replicas: 3`, you have three
standalone brokers. `<cluster>-1` and `<cluster>-2` hold partitions that exist
nowhere else, because nothing ever replicated them. Setting `spec.replicas: 1`
scales the StatefulSet down and those two pods are removed.

- Their `PersistentVolumeClaim`s **survive**: the StatefulSet sets no
  `persistentVolumeClaimRetentionPolicy`, so Kubernetes keeps `data-<cluster>-1`
  and `data-<cluster>-2` after a scale-down and re-attaches them if you ever
  scale back up.
- Any producer or consumer addressed at those pods stops being served. Drain or
  copy that data off first if you need it.

Check what you actually have before patching:

```bash
kubectl get pods -n streamline-system -l app.kubernetes.io/instance=my-cluster
kubectl get pvc -n streamline-system -l app.kubernetes.io/instance=my-cluster
```

To keep the transition under your control, stop the old controller before you
patch, so it cannot act on the new values while you work:

```bash
kubectl scale deployment/streamline-operator -n streamline-system --replicas=0
```

(That is the Deployment name shipped in `deploy/operator.yaml`; adjust it if you
renamed it. Step 6 applies the new Deployment, which restores the replica count.)

## Step 1 — Find every affected resource

List every cluster and topic with the values each one stores. `--all-namespaces`
matters: v0.3.0 watched all of them.

```bash
kubectl get streamlineclusters --all-namespaces \
  -o custom-columns='NAMESPACE:.metadata.namespace,NAME:.metadata.name,REPLICAS:.spec.replicas,PODANTIAFFINITY:.spec.podAntiAffinity'

kubectl get streamlinetopics --all-namespaces \
  -o custom-columns='NAMESPACE:.metadata.namespace,NAME:.metadata.name,REPLICATIONFACTOR:.spec.replicationFactor'
```

Narrow that to just the resources this operator will reject:

```bash
# Clusters that still carry spec.replicas: 3 (or any value above 1)
kubectl get streamlineclusters --all-namespaces -o jsonpath='{range .items[?(@.spec.replicas>1)]}{.metadata.namespace}{"/"}{.metadata.name}{" replicas="}{.spec.replicas}{"\n"}{end}'

# Clusters that still carry spec.podAntiAffinity: true
kubectl get streamlineclusters --all-namespaces -o jsonpath='{range .items[?(@.spec.podAntiAffinity==true)]}{.metadata.namespace}{"/"}{.metadata.name}{"\n"}{end}'

# Topics that still carry spec.replicationFactor: 2 (or any value other than 1)
kubectl get streamlinetopics --all-namespaces -o jsonpath='{range .items[?(@.spec.replicationFactor!=1)]}{.metadata.namespace}{"/"}{.metadata.name}{" replicationFactor="}{.spec.replicationFactor}{"\n"}{end}'

# Topics that still carry spec.retention.retentionMs: 604800000 (or anything else non-default)
kubectl get streamlinetopics --all-namespaces -o jsonpath='{range .items[?(@.spec.retention.retentionMs!=-1)]}{.metadata.namespace}{"/"}{.metadata.name}{" retentionMs="}{.spec.retention.retentionMs}{"\n"}{end}'

# Topics that still carry spec.config.minInsyncReplicas at all (any value is rejected)
kubectl get streamlinetopics --all-namespaces -o jsonpath='{range .items[?(@.spec.config.minInsyncReplicas)]}{.metadata.namespace}{"/"}{.metadata.name}{" minInsyncReplicas="}{.spec.config.minInsyncReplicas}{"\n"}{end}'

# Topics that still carry spec.config.maxMessageBytes at all
kubectl get streamlinetopics --all-namespaces -o jsonpath='{range .items[?(@.spec.config.maxMessageBytes)]}{.metadata.namespace}{"/"}{.metadata.name}{" maxMessageBytes="}{.spec.config.maxMessageBytes}{"\n"}{end}'
```

A `jsonpath` filter skips items where the path does not resolve, which is what
you want here: the last three queries list exactly the topics whose stored spec
opened a `retention:` or `config:` block. Topics that never did are not affected
and do not appear.

The two `spec.config` filters are existence checks, not comparisons, because
**every** value is rejected — including the `1` and `1048576` that v0.3.0 wrote.
For a single view of everything in `spec.config`, including keys this guide does
not patch (`segmentBytes`, `indexIntervalBytes`, `flushIntervalMs`,
`flushMessages`, `custom`), each of which is also rejected and must be removed:

```bash
kubectl get streamlinetopics --all-namespaces \
  -o custom-columns='NAMESPACE:.metadata.namespace,NAME:.metadata.name,CONFIG:.spec.config,RETENTIONMS:.spec.retention.retentionMs'
```

Each command prints nothing when there is nothing left to fix. Keep the output:
it is the work list for Step 4 and the expected-empty check for Step 5.

If the list contains resources outside `streamline-system`, patch them anyway —
they are still stored with the rejected values — but note that the shipped
operator watches only its own namespace, so nothing there will reconcile until
you either deploy an operator into that namespace or opt into cluster-wide mode
with the RBAC described in the README's "Namespace scope and RBAC".

## Step 2 — Install the corrected CRDs first

```bash
kubectl apply -k deploy/crds/
```

This is deliberately **before** the patches, and it is not optional for the two
`spec.config` removals:

- v0.3.0 declares `default: 1` on `minInsyncReplicas` and `default: 1048576` on
  `maxMessageBytes`. Structural defaulting runs *after* a patch is merged, so
  while those CRDs are installed a merge-patch `null` deletes the key and the
  API server re-defaults it back on the very same write. The patch appears to
  succeed and changes nothing.
- The corrected CRDs make both fields optional with no default, so the removal
  sticks.

The four value patches (`replicas`, `podAntiAffinity`, `replicationFactor`,
`retentionMs`) are accepted by both schemas and work in either order — v0.3.0
declares no range constraint on `retentionMs`, so an explicit `-1` is valid
there too.

Installing CRDs is safe at this point:

- It does **not** rewrite stored objects, delete anything, or start the new
  controller. Only the schema used for future writes and reads changes.
- It **does** enable pruning of `spec.replication` and `spec.config.flushMs` on
  the next write to each object. Copy anything you still need out of those
  fields first — see
  [Fields v0.3.0 had that this tree does not](#fields-v030-had-that-this-tree-does-not).

## Step 3 — Dry-run the patches

`--dry-run=server` sends the patch through the API server's admission and
defaulting path and returns the object that *would* be stored, without storing
it.

```bash
kubectl patch streamlineclusters my-cluster -n streamline-system --type merge \
  -p '{"spec":{"replicas":1,"podAntiAffinity":false}}' \
  --dry-run=server -o yaml | grep -E '^  (replicas|podAntiAffinity):'
```

Expected output — the stored object would carry the supported values:

```text
  podAntiAffinity: false
  replicas: 1
```

```bash
kubectl patch streamlinetopics events -n streamline-system --type merge \
  -p '{"spec":{"replicationFactor":1}}' \
  --dry-run=server -o yaml | grep -E '^  replicationFactor:'
```

```text
  replicationFactor: 1
```

The removals are the ones worth rehearsing, because a dry run is exactly how you
prove Step 2 actually took effect. Ask for the whole `spec.config` back:

```bash
kubectl patch streamlinetopics orders -n streamline-system --type merge \
  -p '{"spec":{"config":{"minInsyncReplicas":null}}}' \
  --dry-run=server -o jsonpath='{.spec.config}'
```

The key must be **absent** from the output. If it comes back as
`"minInsyncReplicas":1`, the v0.3.0 CRDs are still installed and Step 2 has not
been applied — re-run it before continuing.

If a dry run still shows the old value, the field name is wrong or you patched
the wrong namespace — fix that before running Step 4.

## Step 4 — Patch the stored objects

One command per field, so each change is legible in an audit log and in
`kubectl` history. Substitute your own names and namespaces; every example here
uses `streamline-system`, the namespace the shipped deployment watches.

```bash
# StreamlineCluster: spec.replicas 3 -> 1
kubectl patch streamlineclusters my-cluster -n streamline-system --type merge -p '{"spec":{"replicas":1}}'

# StreamlineCluster: spec.podAntiAffinity true -> false
kubectl patch streamlineclusters my-cluster -n streamline-system --type merge -p '{"spec":{"podAntiAffinity":false}}'

# StreamlineTopic: spec.replicationFactor 2 -> 1
kubectl patch streamlinetopics events -n streamline-system --type merge -p '{"spec":{"replicationFactor":1}}'

# StreamlineTopic: spec.retention.retentionMs 604800000 -> -1 (unlimited)
kubectl patch streamlinetopics orders -n streamline-system --type merge -p '{"spec":{"retention":{"retentionMs":-1}}}'

# StreamlineTopic: remove spec.config.minInsyncReplicas (null deletes the key)
kubectl patch streamlinetopics orders -n streamline-system --type merge -p '{"spec":{"config":{"minInsyncReplicas":null}}}'

# StreamlineTopic: remove spec.config.maxMessageBytes
kubectl patch streamlinetopics orders -n streamline-system --type merge -p '{"spec":{"config":{"maxMessageBytes":null}}}'
```

Each patch is nested down to the leaf on purpose. `{"spec":{"config":null}}`
would clear the rejections too — and would delete `segmentBytes`,
`flushIntervalMs`, `custom`, and everything else in the block along with them.
Under RFC 7386 (what `--type merge` sends) the nested form replaces only the
keys it names and leaves every sibling alone: `spec.retention.cleanupPolicy`
survives the `retentionMs` patch, and `maxMessageBytes` survives the
`minInsyncReplicas` removal.

Both cluster fields in one write, if you prefer a single patch per object:

```bash
kubectl patch streamlineclusters my-cluster -n streamline-system --type merge \
  -p '{"spec":{"replicas":1,"podAntiAffinity":false}}'
```

The same works for a topic — one write, all four corrections:

```bash
kubectl patch streamlinetopics orders -n streamline-system --type merge \
  -p '{"spec":{"replicationFactor":1,"retention":{"retentionMs":-1},"config":{"minInsyncReplicas":null,"maxMessageBytes":null}}}'
```

For more than a handful of resources, iterate over the namespace/name pairs from
Step 1 rather than assuming a namespace:

```bash
kubectl get streamlineclusters --all-namespaces \
  -o jsonpath='{range .items[*]}{.metadata.namespace}{" "}{.metadata.name}{"\n"}{end}' |
while read -r ns name; do
  kubectl patch streamlineclusters "$name" -n "$ns" --type merge \
    -p '{"spec":{"replicas":1,"podAntiAffinity":false}}'
done

kubectl get streamlinetopics --all-namespaces \
  -o jsonpath='{range .items[*]}{.metadata.namespace}{" "}{.metadata.name}{"\n"}{end}' |
while read -r ns name; do
  kubectl patch streamlinetopics "$name" -n "$ns" --type merge \
    -p '{"spec":{"replicationFactor":1,"retention":{"retentionMs":-1},"config":{"minInsyncReplicas":null,"maxMessageBytes":null}}}'
done
```

Update the manifests in your Git repository to match, with the values written
out explicitly:

```yaml
apiVersion: streamline.io/v1alpha1
kind: StreamlineCluster
metadata:
  name: my-cluster
  namespace: streamline-system
spec:
  replicas: 1
  podAntiAffinity: false
  storage:
    size: 10Gi
```

```yaml
apiVersion: streamline.io/v1alpha1
kind: StreamlineTopic
metadata:
  name: events
  namespace: streamline-system
spec:
  clusterRef: my-cluster
  partitions: 6
  replicationFactor: 1
  retention:
    retentionMs: -1
    retentionBytes: -1
    cleanupPolicy: delete
```

Delete any `config:` block from the manifest rather than emptying it: every key
in it is rejected, and `config: {}` is enough for a CRD that defaults those keys
to put them back.

Leaving the fields out works too once the corrected CRDs are installed — the new
schema defaults are `spec.replicas: 1`, `spec.podAntiAffinity: false`,
`spec.replicationFactor: 1`, and `spec.retention.retentionMs: -1`, and the
`spec.config` keys have no defaults at all — but writing them explicitly makes a
re-apply against an old CRD harmless.

## Step 5 — Verify before deploying

Re-run the six filtered queries from Step 1. All six must print nothing:

```bash
kubectl get streamlineclusters --all-namespaces -o jsonpath='{range .items[?(@.spec.replicas>1)]}{.metadata.namespace}{"/"}{.metadata.name}{"\n"}{end}'
kubectl get streamlineclusters --all-namespaces -o jsonpath='{range .items[?(@.spec.podAntiAffinity==true)]}{.metadata.namespace}{"/"}{.metadata.name}{"\n"}{end}'
kubectl get streamlinetopics --all-namespaces -o jsonpath='{range .items[?(@.spec.replicationFactor!=1)]}{.metadata.namespace}{"/"}{.metadata.name}{"\n"}{end}'
kubectl get streamlinetopics --all-namespaces -o jsonpath='{range .items[?(@.spec.retention.retentionMs!=-1)]}{.metadata.namespace}{"/"}{.metadata.name}{"\n"}{end}'
kubectl get streamlinetopics --all-namespaces -o jsonpath='{range .items[?(@.spec.config.minInsyncReplicas)]}{.metadata.namespace}{"/"}{.metadata.name}{"\n"}{end}'
kubectl get streamlinetopics --all-namespaces -o jsonpath='{range .items[?(@.spec.config.maxMessageBytes)]}{.metadata.namespace}{"/"}{.metadata.name}{"\n"}{end}'
```

Confirm the values that are now stored:

```bash
kubectl get streamlineclusters --all-namespaces \
  -o custom-columns='NAMESPACE:.metadata.namespace,NAME:.metadata.name,REPLICAS:.spec.replicas,PODANTIAFFINITY:.spec.podAntiAffinity'

kubectl get streamlinetopics --all-namespaces \
  -o custom-columns='NAMESPACE:.metadata.namespace,NAME:.metadata.name,REPLICATIONFACTOR:.spec.replicationFactor,RETENTIONMS:.spec.retention.retentionMs,CONFIG:.spec.config'
```

`CONFIG` should read `map[custom:map[]]` or `<none>` — anything else is a
setting the operator still rejects.

## Step 6 — Deploy the controller

The CRDs went in at Step 2. What is left is the namespace, RBAC, and the
operator itself:

```bash
# 1. Namespace and RBAC
kubectl apply -f deploy/namespace.yaml
kubectl apply -f deploy/rbac/

# 2. The operator. deploy/operator.yaml ships an unpullable placeholder image
#    on purpose, so render a deployable manifest from an explicit, immutable
#    digest:
make release-manifests IMAGE=ghcr.io/streamlinelabs/streamline-operator@sha256:<digest>
kubectl apply -f deploy/operator.release.yaml
```

If you scaled the old controller to zero before patching, this restores a
running Deployment at the new image.

Then confirm the operator adopted the patched resources instead of rejecting
them:

```bash
kubectl get streamlineclusters -n streamline-system \
  -o custom-columns='NAME:.metadata.name,PHASE:.status.phase,READY:.status.readyReplicas'

kubectl get streamlinetopics -n streamline-system \
  -o custom-columns='NAME:.metadata.name,PHASE:.status.phase,READY:.status.ready'
```

A patched cluster reaches `phase: Running`; a patched topic reaches
`phase: Ready`. Neither should report `Failed`.

## What a missed resource looks like

The rejection is explicit, names the field, and repeats the patch, so a resource
found later needs no archaeology:

```bash
kubectl describe streamlinecluster my-cluster -n streamline-system
```

```text
Status:
  Phase:  Failed
  Conditions:
    Type:     Ready
    Status:   False
    Reason:   InvalidSpec
    Message:  replicas=3 is not supported: the operator does not bootstrap Streamline
              raft peers yet (set replicas: 1), so 3 independent brokers would be
              rendered instead of a quorum. Set spec.replicas: 1 and re-apply, or patch
              the stored object in place with
              `kubectl patch streamlineclusters <name> -n <namespace> --type merge -p '{"spec":{"replicas":1}}'`.
              It is the v0.3.0 CRD schema default, which the API server persisted into
              every StreamlineCluster created against those CRDs, so the spec can carry
              it without anyone having written it. The operator does not rewrite specs.
              See docs/UPGRADING.md for the full upgrade path, including how to find
              every affected resource.
```

The same wording appears for `spec.podAntiAffinity` on a cluster and for
`spec.replicationFactor` on a topic. A topic rejected for one of the nested
defaults reads the same way, with the block that had to exist named explicitly:

```text
Status:
  Phase:  Failed
  Conditions:
    Type:     Ready
    Status:   False
    Reason:   UnsupportedConfiguration
    Message:  config.minInsyncReplicas=1 is not supported: the Streamline topic API
              applies no topic configuration (it accepts only name and partitions), so
              this setting would be silently discarded (leave it at the default unset).
              Remove the key — writing `spec.config.minInsyncReplicas: null` deletes this
              one leaf and leaves every other spec.config setting in place — and
              re-apply, or patch the stored object in place with
              `kubectl patch streamlinetopics <name> -n <namespace> --type merge -p '{"spec":{"config":{"minInsyncReplicas":null}}}'`.
              The removal only sticks once the corrected CRDs are installed: v0.3.0
              declares a default for this field, and defaulting runs after the patch is
              merged, so the API server puts the key back on the same write. It is the
              v0.3.0 CRD schema default, which the API server persisted into every
              StreamlineTopic whose manifest opened a `spec.config` block (structural
              defaults are only injected into objects that are present), so the spec can
              carry it without anyone having written it. The operator does not rewrite
              specs. See docs/UPGRADING.md for the full upgrade path, including how to
              find every affected resource.
```

You can also watch for them in the operator log:

```bash
kubectl logs deployment/streamline-operator -n streamline-system | grep -i "not supported"
```

## Rolling back

Rolling the *controller* back is a plain image change. Rolling the *data* back
is not symmetric, and three things do not come back on their own.

**The four value patches are backward-compatible.** `spec.replicas: 1`,
`spec.podAntiAffinity: false`, `spec.replicationFactor: 1`, and
`spec.retention.retentionMs: -1` are all valid under the v0.3.0 CRDs, so a
patched resource keeps working if you reinstall v0.3.0. Restoring the old values
is another `kubectl patch` with the old numbers, though v0.3.0 will then resume
rendering three independent brokers and re-advertising a seven-day retention
nothing enforces.

**The two removals are re-defaulted by v0.3.0.** Reinstalling the v0.3.0 CRDs
restores `default: 1` on `minInsyncReplicas` and `default: 1048576` on
`maxMessageBytes`, so the next write to each topic — by you, by a controller, or
by `kubectl apply` — puts both keys straight back. There is nothing to undo, but
also no way to keep them absent while those CRDs are installed.

**Pruned fields are gone.** `StreamlineCluster.spec.replication` and
`StreamlineTopic.spec.config.flushMs` are not in this tree's schemas, so any
object written while the corrected CRDs were installed has had them pruned, and
reinstalling the v0.3.0 CRDs does not bring the values back — it only makes the
fields writable again. Restore them from the backup taken in
[Fields v0.3.0 had that this tree does not](#fields-v030-had-that-this-tree-does-not),
or re-apply the original manifests.

The compatible part is the API surface: the group and version
(`streamline.io/v1alpha1`) are unchanged, and every field in the
[What changed](#what-changed) table still exists under the same name. What moved
is the defaults, the two removed fields, and what the controller accepts.

