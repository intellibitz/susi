# GMCP evidence

Current operational proof lives in ignored `.susi/records/gmcp/evidence/`,
one JSONL snapshot per vector (`VC-203-<NNN>.jsonl`). Each snapshot retains only
the latest record per requirement. Historical source JSONL files remain readable
for compatibility; no recorder appends to them. Optional history may be absent.
A present local snapshot takes precedence, including when corrupt or stale;
readers never substitute older source proof to conceal a local failure.

Each record names the requirement, actual check command and exit status,
recording identity and time, ledger digest (`artifact_sha256`), tested source
commit (`tested_commit`) and artifact tree digest (`tested_artifacts`). The
artifact digest binds the vector's production paths, tests, ledger, pinned
specifications, fixtures, contracts and GMCP source. Only the GMCP manifest's
`package.version` is normalized; features and dependencies remain bound.
Source ancestry and exact artifact checks remain required. Missing, malformed,
stale or unbound proof fails qualification; file existence proves no capability.

Run `cargo local-release record-gmcp [VC-203-NNN ...]` on a clean committed tree,
then `cargo local-release record-gmcp --mastery [VC-203-NNN ...]` on the same tree.
The shell compatibility entry point is `scripts/gmcp-record-evidence.sh`.
Both phases record only after every selected test actually runs and passes,
and refuse changed source or concurrent evidence changes. Local files use
atomic replacement, reject redirected paths and are bounded to 16 MiB each.

A new mastery record additionally binds `requirements_sha256`, the SHA-256 of
canonical JSON mapping requirement IDs to the exact latest requirement records
that its test read. This detects a later replacement even on the same source
commit. Legacy mastery records retain their committed-input comparison; legacy
supersession chains are still validated. Mastery never consumes its own output.

Local snapshots are operational state, not release certificates. Preserve the
required source, test, toolchain, artifact and license provenance with a release
artifact independently of these optional local records.
