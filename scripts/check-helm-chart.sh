#!/usr/bin/env bash
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
chart=deploy/helm/susi
test -f "$chart/Chart.yaml"
test -f "$chart/values.yaml"
test -f "$chart/templates/deployment.yaml"
if command -v helm >/dev/null 2>&1; then
  helm lint "$chart"
  helm template susi "$chart" >/dev/null
else
  grep -q '^name: susi' "$chart/Chart.yaml"
  grep -q 'kind: Deployment' "$chart/templates/deployment.yaml"
fi
echo "helm: ok"
