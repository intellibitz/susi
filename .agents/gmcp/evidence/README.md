# GMCP evidence

Immutable evidence artifacts for the VC-203 program. Convention (see
`docs/gmcp/contract-203-001.md`):

- One append-only JSONL file per vector: `VC-203-<NNN>.jsonl`.
- Each line records one retained proof event: the ledger row id it satisfies,
  the check that ran (command + exit), the artifact hash, and the recording
  identity — enough for an independent reader to re-derive the verdict.
- Records are never edited or deleted; a superseded proof is a new line that
  names the record it replaces in `supersedes` (its `recorded_unix`,
  `identity` and `artifact_sha256`). A requirement is judged by its newest
  record: the one no later record supersedes, the latest `recorded_unix`
  among those.
- Each record binds to the code its check ran on: `tested_commit` is that
  commit, and `tested_artifacts` the sha256 of
  `git ls-tree -r --full-tree <tested_commit> -- <artifacts>` over
  `tests/gmcp_requirements.rs`, the ledger and the vector's existing
  production paths. A record whose digest is no longer HEAD's is stale: a
  change to the tests, the ledger or the vector's production code needs its
  checks run again and re-recorded. `scripts/gmcp-record-evidence.sh` does
  that on a clean HEAD, appending records only when every check passes.
- `susi tasks audit` and `gmcp_vc_203_<NNN>_mastery` read these files; a
  missing, malformed, stale or unbound record fails the mastery gate closed.
