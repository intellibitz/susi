# GMCP evidence

Immutable evidence artifacts for the VC-203 program. Convention (see
`docs/gmcp/contract-203-001.md`):

- One append-only JSONL file per vector: `VC-203-<NNN>.jsonl`.
- Each line records one retained proof event: the ledger row id it satisfies,
  the check that ran (command + exit), the artifact hash, and the recording
  identity — enough for an independent reader to re-derive the verdict.
- Records are never edited or deleted; a superseded proof is a new line that
  names the record it replaces.
- `susi tasks audit` and `gmcp_vc_203_<NNN>_mastery` read these files; a
  missing or malformed record fails the mastery gate closed.
