# Operator Troubleshooting

## CRD Issues

### StreamlineCluster not reconciling
- Check operator logs: `kubectl logs -l app=streamline-operator`
- Verify CRDs are installed: `kubectl get crds | grep streamline`
- Check RBAC permissions for the operator service account

### Pod not starting
- Check resource limits are sufficient
- Verify the Streamline Docker image is accessible
- Check PersistentVolumeClaims are bound

## Scaling Issues

### Autoscaler not scaling up
- Verify HPA metrics are available
- Check that the metrics server is running
- Review autoscaling thresholds in StreamlineCluster spec

## Network Issues

### Clients cannot connect to cluster
- Verify Service is created: `kubectl get svc -l app=streamline`
- Check NetworkPolicy allows traffic on port 9092
- For external access, verify LoadBalancer or Ingress configuration
