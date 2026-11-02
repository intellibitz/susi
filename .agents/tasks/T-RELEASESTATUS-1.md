# T-RELEASESTATUS-1: measured native status for release automation

Owner: release-status. Base: a4dc8fc72204f29ac1c7a7d5fda2aa2075316764.
Dependency: accepted native Linux candidate and qualified production A.
Scopes: src/cli/defs.rs, src/cli/mission_cli.rs, src/cli/status_cli.rs,
tests/unified_surface_status.rs, this task record.

Add susi status --json to the shipped executable. Agents need a deterministic
machine-readable health snapshot and the actual executable digest to verify
installed identity across promotion and rollback. Existing prose status stays
compatible. JSON must not print the substrate identity key or provider secrets.
Failure to read/hash the executable fails the command; never invent identity.

Acceptance: real-binary JSON parsing, independently hashed executable bytes,
stopped/running daemon state and selected model; no composition/inference for
status. Full integrated-tree gates and production qualification are required.
Measure A rejecting the new option and B succeeding, then run genuine A/B/A
promotion, installed health, digest verification and restored A inference.
