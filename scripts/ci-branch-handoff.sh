#!/usr/bin/env bash
# Retired: GitHub Actions cannot substitute for the native local gate.
printf '%s\n' 'Branch CI handoff is retired; run cargo local-workflow check locally, then cargo local-workflow ship <name> for serialized publication.' >&2
exit 1
