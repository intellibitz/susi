#!/usr/bin/env bash
# Compatibility entry point for the native GMCP recorder.
# Both requirement and gmcp_vc_ mastery checks bind artifact_sha256 and the
# tested commit. Records stay under ignored .susi/records/gmcp/evidence;
# source history is read only for compatibility and is never appended.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
exec cargo local-release record-gmcp "$@"
