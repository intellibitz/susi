#!/usr/bin/env bash
# Helm chart contract: the susi chart exists, carries daemon + engine +
# GPU-node + secrets surfaces, and renders/lints when helm is available.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CHART="$ROOT/helm/susi"

[[ -f "$CHART/Chart.yaml" ]] || { echo "missing helm/susi/Chart.yaml" >&2; exit 1; }
[[ -f "$CHART/values.yaml" ]] || { echo "missing helm/susi/values.yaml" >&2; exit 1; }

grep -q '^name: susi' "$CHART/Chart.yaml"
grep -q '^apiVersion: v2' "$CHART/Chart.yaml"

# Daemon deployment with health probe and a model-store volume.
grep -q 'susi-daemon' "$CHART/templates/deployment.yaml"
grep -q 'readinessProbe' "$CHART/templates/deployment.yaml"
grep -q 'model-store' "$CHART/templates/deployment.yaml"

# Engine sidecars and GPU node placement.
grep -q 'engines.ollama' "$CHART/templates/deployment.yaml"
grep -q 'engines.vllm' "$CHART/templates/deployment.yaml"
grep -q 'nodeSelector' "$CHART/templates/deployment.yaml"
grep -q 'resourceName' "$CHART/values.yaml"

# Secrets come from Kubernetes (existing Secret ref or chart-created),
# never a committed plaintext value.
grep -q 'secretRef' "$CHART/templates/deployment.yaml"
grep -q 'existing' "$CHART/values.yaml"

# Service + persistence + operator surfaces exist.
for t in service.yaml pvc.yaml crd-susistack.yaml operator.yaml; do
  [[ -f "$CHART/templates/$t" ]] || { echo "missing templates/$t" >&2; exit 1; }
done
grep -q 'susistacks.susi.dev' "$CHART/templates/crd-susistack.yaml"

# When helm is installed, lint + render for real.
if command -v helm >/dev/null 2>&1; then
  helm lint "$CHART" >/dev/null
  helm template susi "$CHART" >/dev/null
  helm template susi "$CHART" --set gpu.enabled=true \
      --set engines.vllm.enabled=true --set secrets.existing=susi-keys >/dev/null
  echo "check-helm-chart: ok (helm lint+template)"
else
  echo "check-helm-chart: ok (structural; helm not installed)"
fi
