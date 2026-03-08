# Operator API Reference

## Custom Resource Definitions

### StreamlineCluster

```yaml
apiVersion: streamline.io/v1alpha1
kind: StreamlineCluster
metadata:
  name: my-cluster
spec:
  replicas: 3
  version: "0.3.0"
  storage:
    size: 10Gi
    storageClass: standard
  resources:
    requests:
      cpu: 500m
      memory: 1Gi
    limits:
      cpu: 2
      memory: 4Gi
```

### StreamlineTopic

```yaml
apiVersion: streamline.io/v1alpha1
kind: StreamlineTopic
metadata:
  name: events
spec:
  clusterRef: my-cluster
  partitions: 6
  retentionMs: 604800000  # 7 days
```
