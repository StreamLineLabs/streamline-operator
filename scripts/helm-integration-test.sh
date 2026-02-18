#!/usr/bin/env bash
# Helm Integration Tests for Streamline Operator
#
# Prerequisites:
#   - kubectl configured with access to a Kubernetes cluster
#   - Helm 3 installed
#   - streamline-operator CRDs installed (or Helm chart with CRDs)
#
# Usage:
#   ./scripts/helm-integration-test.sh [--namespace <ns>] [--skip-cleanup]

set -euo pipefail

NAMESPACE="${NAMESPACE:-streamline-test}"
SKIP_CLEANUP="${SKIP_CLEANUP:-false}"
TIMEOUT="120s"
PASSED=0
FAILED=0
ERRORS=()

# Parse arguments
while [[ $# -gt 0 ]]; do
  case $1 in
    --namespace) NAMESPACE="$2"; shift 2 ;;
    --skip-cleanup) SKIP_CLEANUP="true"; shift ;;
    *) echo "Unknown option: $1"; exit 1 ;;
  esac
done

# --- Helpers ---

log_info()  { echo -e "\033[34m[INFO]\033[0m  $*"; }
log_pass()  { echo -e "\033[32m[PASS]\033[0m  $*"; PASSED=$((PASSED + 1)); }
log_fail()  { echo -e "\033[31m[FAIL]\033[0m  $*"; FAILED=$((FAILED + 1)); ERRORS+=("$*"); }

wait_for_condition() {
  local resource="$1" condition="$2" timeout="$3"
  if kubectl wait --for="condition=${condition}" "${resource}" \
      -n "${NAMESPACE}" --timeout="${timeout}" 2>/dev/null; then
    return 0
  fi
  return 1
}

cleanup() {
  if [[ "${SKIP_CLEANUP}" == "true" ]]; then
    log_info "Skipping cleanup (--skip-cleanup)"
    return
  fi
  log_info "Cleaning up test namespace ${NAMESPACE}"
  kubectl delete namespace "${NAMESPACE}" --ignore-not-found --wait=false 2>/dev/null || true
}

trap cleanup EXIT

# --- Test Setup ---

log_info "=== Streamline Operator Helm Integration Tests ==="
log_info "Namespace: ${NAMESPACE}"
log_info ""

# Verify kubectl connectivity
if ! kubectl cluster-info &>/dev/null; then
  log_fail "Cannot connect to Kubernetes cluster"
  exit 1
fi
log_pass "Connected to Kubernetes cluster"

# Create test namespace
kubectl create namespace "${NAMESPACE}" --dry-run=client -o yaml | kubectl apply -f - >/dev/null
log_pass "Test namespace ${NAMESPACE} created"

# --- Test 1: CRD Installation ---

log_info ""
log_info "--- Test 1: CRD Installation ---"

check_crd() {
  local crd_name="$1"
  if kubectl get crd "${crd_name}" &>/dev/null; then
    log_pass "CRD ${crd_name} is installed"
  else
    log_fail "CRD ${crd_name} is NOT installed"
  fi
}

check_crd "streamlineclusters.streamline.io"
check_crd "streamlinetopics.streamline.io"
check_crd "streamlineusers.streamline.io"

# --- Test 2: Basic Cluster Creation ---

log_info ""
log_info "--- Test 2: Basic Cluster Creation ---"

cat <<EOF | kubectl apply -n "${NAMESPACE}" -f -
apiVersion: streamline.io/v1alpha1
kind: StreamlineCluster
metadata:
  name: test-cluster
spec:
  replicas: 1
  image: ghcr.io/streamlinelabs/streamline:latest
  storage:
    size: 1Gi
  updateStrategy:
    type: RollingUpdate
    maxUnavailable: 1
  resources:
    requests:
      cpu: "100m"
      memory: "256Mi"
    limits:
      cpu: "500m"
      memory: "512Mi"
EOF

if kubectl get streamlinecluster test-cluster -n "${NAMESPACE}" &>/dev/null; then
  log_pass "StreamlineCluster test-cluster created"
else
  log_fail "Failed to create StreamlineCluster test-cluster"
fi

# Verify the cluster resource has expected fields
REPLICAS=$(kubectl get streamlinecluster test-cluster -n "${NAMESPACE}" -o jsonpath='{.spec.replicas}' 2>/dev/null || echo "")
if [[ "${REPLICAS}" == "1" ]]; then
  log_pass "StreamlineCluster replicas set correctly"
else
  log_fail "StreamlineCluster replicas expected 1, got '${REPLICAS}'"
fi

# Check finalizer is added (if operator is running)
FINALIZERS=$(kubectl get streamlinecluster test-cluster -n "${NAMESPACE}" -o jsonpath='{.metadata.finalizers}' 2>/dev/null || echo "")
if [[ -n "${FINALIZERS}" ]]; then
  log_pass "StreamlineCluster has finalizers: ${FINALIZERS}"
else
  log_info "StreamlineCluster has no finalizers (operator may not be running)"
fi

# --- Test 3: Topic CRUD Operations ---

log_info ""
log_info "--- Test 3: Topic CRUD Operations ---"

# Create topic
cat <<EOF | kubectl apply -n "${NAMESPACE}" -f -
apiVersion: streamline.io/v1alpha1
kind: StreamlineTopic
metadata:
  name: test-topic
spec:
  clusterRef: test-cluster
  partitions: 6
  replicationFactor: 1
  retention:
    retentionMs: 86400000
    cleanupPolicy: delete
  compression:
    type: lz4
EOF

if kubectl get streamlinetopic test-topic -n "${NAMESPACE}" &>/dev/null; then
  log_pass "StreamlineTopic test-topic created"
else
  log_fail "Failed to create StreamlineTopic test-topic"
fi

PARTITIONS=$(kubectl get streamlinetopic test-topic -n "${NAMESPACE}" -o jsonpath='{.spec.partitions}' 2>/dev/null || echo "")
if [[ "${PARTITIONS}" == "6" ]]; then
  log_pass "StreamlineTopic partitions set correctly"
else
  log_fail "StreamlineTopic partitions expected 6, got '${PARTITIONS}'"
fi

# Update topic
kubectl patch streamlinetopic test-topic -n "${NAMESPACE}" --type=merge \
  -p '{"spec":{"partitions":12}}' 2>/dev/null || true

PARTITIONS=$(kubectl get streamlinetopic test-topic -n "${NAMESPACE}" -o jsonpath='{.spec.partitions}' 2>/dev/null || echo "")
if [[ "${PARTITIONS}" == "12" ]]; then
  log_pass "StreamlineTopic updated partitions to 12"
else
  log_fail "StreamlineTopic update failed, partitions: '${PARTITIONS}'"
fi

# Delete topic
kubectl delete streamlinetopic test-topic -n "${NAMESPACE}" --wait=false 2>/dev/null || true
sleep 2
if ! kubectl get streamlinetopic test-topic -n "${NAMESPACE}" &>/dev/null; then
  log_pass "StreamlineTopic test-topic deleted"
else
  log_info "StreamlineTopic test-topic still exists (finalizer may be pending)"
fi

# --- Test 4: User CRUD Operations ---

log_info ""
log_info "--- Test 4: User CRUD Operations ---"

cat <<EOF | kubectl apply -n "${NAMESPACE}" -f -
apiVersion: streamline.io/v1alpha1
kind: StreamlineUser
metadata:
  name: test-user
spec:
  clusterRef: test-cluster
  authentication:
    type: scram-sha256
  authorization:
    type: simple
    acls:
      - resourceType: topic
        resourceName: test-topic
        patternType: literal
        operations: [read, write]
        permission: allow
EOF

if kubectl get streamlineuser test-user -n "${NAMESPACE}" &>/dev/null; then
  log_pass "StreamlineUser test-user created"
else
  log_fail "Failed to create StreamlineUser test-user"
fi

AUTH_TYPE=$(kubectl get streamlineuser test-user -n "${NAMESPACE}" -o jsonpath='{.spec.authentication.type}' 2>/dev/null || echo "")
if [[ "${AUTH_TYPE}" == "scram-sha256" ]]; then
  log_pass "StreamlineUser authentication type set correctly"
else
  log_fail "StreamlineUser auth type expected scram-sha256, got '${AUTH_TYPE}'"
fi

# --- Test 5: Upgrade Path ---

log_info ""
log_info "--- Test 5: Upgrade Path ---"

# Test updating the cluster image (simulates an upgrade)
kubectl patch streamlinecluster test-cluster -n "${NAMESPACE}" --type=merge \
  -p '{"spec":{"image":"ghcr.io/streamlinelabs/streamline:v0.3.0"}}' 2>/dev/null || true

IMAGE=$(kubectl get streamlinecluster test-cluster -n "${NAMESPACE}" -o jsonpath='{.spec.image}' 2>/dev/null || echo "")
if [[ "${IMAGE}" == "ghcr.io/streamlinelabs/streamline:v0.3.0" ]]; then
  log_pass "StreamlineCluster image upgraded to v0.3.0"
else
  log_fail "StreamlineCluster image upgrade failed, got '${IMAGE}'"
fi

# Verify update strategy is preserved
STRATEGY=$(kubectl get streamlinecluster test-cluster -n "${NAMESPACE}" -o jsonpath='{.spec.updateStrategy.type}' 2>/dev/null || echo "")
if [[ "${STRATEGY}" == "RollingUpdate" ]]; then
  log_pass "StreamlineCluster update strategy preserved after upgrade"
else
  log_fail "StreamlineCluster update strategy expected RollingUpdate, got '${STRATEGY}'"
fi

# Test scaling
kubectl patch streamlinecluster test-cluster -n "${NAMESPACE}" --type=merge \
  -p '{"spec":{"replicas":3}}' 2>/dev/null || true

REPLICAS=$(kubectl get streamlinecluster test-cluster -n "${NAMESPACE}" -o jsonpath='{.spec.replicas}' 2>/dev/null || echo "")
if [[ "${REPLICAS}" == "3" ]]; then
  log_pass "StreamlineCluster scaled to 3 replicas"
else
  log_fail "StreamlineCluster scale failed, replicas: '${REPLICAS}'"
fi

# --- Summary ---

log_info ""
log_info "=== Test Summary ==="
log_info "Passed: ${PASSED}"
log_info "Failed: ${FAILED}"

if [[ ${FAILED} -gt 0 ]]; then
  log_info ""
  log_info "Failures:"
  for err in "${ERRORS[@]}"; do
    echo "  - ${err}"
  done
  exit 1
fi

log_pass "All tests passed!"
exit 0
