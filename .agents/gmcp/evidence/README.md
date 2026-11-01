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
- Each record binds to the code its check passed on: `tested_commit` is that
  commit, and `tested_artifacts` the sha256 of
  `git ls-tree -r --full-tree <tested_commit> -- <artifacts>` over the files
  the checks read: the vector's existing production paths,
  `tests/gmcp_requirements.rs`, the ledger, `sources.json`, `spec/`,
  `fixtures/`, `docs/gmcp`, `docs/gmcp-roadmap.md`,
  `scripts/check-gmcp-ledger.sh` and `crates/susi-gmcp/src`. The raw GMCP
  manifest tree row is replaced with the marker `susi-gmcp manifest without
  package.version` plus a newline and its committed content, omitting only
  `version = ...` inside `[package]`. Features and dependency specifications
  remain bound; an ordinary package release number does not stale evidence. A record whose digest is no
  longer HEAD's is stale; in a full clone `tested_commit` must also exist
  and be in HEAD's history.
- `scripts/gmcp-record-evidence.sh` re-records, in two phases on a clean
  HEAD: first the requirement checks (commit what it appends), then
  `--mastery`, which runs the mastery tests on that commit and records the
  `-M` records only if they pass. A mastery test never reads its own `-M`
  record, so that record states exactly what the test proved;
  `gmcp_mastery_records_are_retained_and_current` checks it instead: one
  newest `-M` record per vector, naming the vector's mastery test, bound and
  current, and (in a full clone) recorded at a commit whose evidence holds
  exactly the requirement records HEAD judges by. Recording the requirement
  checks without the `--mastery` phase fails it.
- `susi tasks audit` and `gmcp_vc_203_<NNN>_mastery` read these files; a
  missing, malformed, stale or unbound record fails the mastery gate closed.
