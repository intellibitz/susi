# Portable feature and release distillation

Development `main` is an unqualified integration stream where independent agents
publish cohesive, labelled changes rapidly. Release branches and release snapshots
must contain only selected contributions and their verified dependency closure,
composed cleanly onto a pinned base commit with auditable provenance.

`local-distill` implements the portable Git-native distillation and candidate
composition slice required by Mandate 73. It uses Rust and the installed Git
executable only, with zero GitHub API, Actions, pull-request or third-party
coordinator prerequisites.

## Plan and compose distilled branches

From a Susi checkout or isolated agent worktree:

```text
cargo build --offline --locked -p xtask --bin local-distill --bin push-policy
cargo run --offline --locked -p xtask --bin local-distill -- plan --name 2026-11-11-rc1 --base <base-rev> --from <stream-rev> --select T-EXAMPLE-1 --json
cargo run --offline --locked -p xtask --bin local-distill -- compose --name 2026-11-11-rc1 --base <base-rev> --from <stream-rev> --select T-EXAMPLE-1 --verify-accept --release-candidate --strict
cargo run --offline --locked -p xtask --bin local-distill -- orchestrate --name 2026-11-11-rc1 --base <base-rev> --from <stream-rev> --tasks-from-range <base-rev>..<stream-rev> --verify-accept --publish-snapshot --strict
cargo run --offline --locked -p xtask --bin local-distill -- submit-main --rev HEAD --json
cargo run --offline --locked -p xtask --bin local-distill -- status 2026-11-11-rc1
cargo run --offline --locked -p xtask --bin local-distill -- publish 2026-11-11-rc1
```

`local-distill` complements the plan-only `distill` binary (`docs/distillation.md`),
can directly consume a `susi-distill-plan/v1` JSON file via `--plan-manifest <path>`,
and supports self-contained `orchestrate` + `--tasks-from-range <A..B>` automatic
`Task:` trailer discovery and `submit-main` non-force object-store reconciliation
onto `refs/heads/main` with zero dependency on any external agent or coordinator.
In `susi-harness`, `susi_harness::recursive` (`susi-recursive-container-v1`) lets
any verified container act as Driver (coordinator), Engine (worker), or both across
nested waves with isolated sandboxes and `IntegrationPolicy::fast_main()` freedom.
Feature manifests (`schema: "susi-feature-manifest-v1"`) can also drive selection
via `--feature <path-or-rev:path>`, binding `id`, `title`, `license`,
`destination_branch`, `enabled_capabilities`, `disabled_capabilities`, `tasks`
and `deps`. Feature licenses are checked against Susi's allowlist (`Apache-2.0`,
`MIT`, `BSD-2-Clause` and `BSD-3-Clause`).

## Selection, closure and composition guarantees

1. **Git object-store reads only:** Task manifests (`.agents/tasks/*.json`,
   `.agents/tasks/done/*.json` and `.agents/tasks/in-progress/*.json`) and commit
   histories are read directly from the resolved `--base` and `--from` commits via
   batched `git ls-tree`, `git cat-file --batch` and single-pass `git log --raw -z`,
   never from uncommitted working-tree files. Modified commit graphs (`info/grafts`
   or `GIT_GRAFT_FILE`) fail closed.
2. **Bounded transitive closure and cycle rejection:** Requested tasks expand
   transitively across `deps` up to depth `256` in deterministic topological
   order. Dependency cycles are rejected with `cyclic_dependency`, and tasks
   depending on a rejected prerequisite are rejected with `dependency_rejected`.
   Prerequisites already present in `--base` with zero newer commits are recorded
   under `satisfied_in_base`.
3. **Scope containment and confidentiality:** Every attributed commit in
   `base..from` is checked with `git diff-tree` against the task's declared
   `scopes`. Modifications outside `scopes` fail with `out_of_scope_diff`;
   credential or private-key filenames (`.env`, `credentials.json`, `*.pem`,
   `*.key`, `*.p12`) fail with `confidentiality_violation`.
4. **Selective 3-way composition without unselected leaks:** Each accepted task
   is applied onto the running candidate using
   `git merge-tree --write-tree --merge-base=<commit-parent> <candidate> <commit>`
   (with an automatic synthetic-base object-store `commit-tree` + `merge-tree
   --write-tree` fallback on Git 2.38–2.39 hosts without `--merge-base=`),
   carrying only that task's delta rather than merging intermediate unselected
   commits from `main`. Conflicting tasks (exit status `1`) are isolated and
   recorded under `deferred` (`merge_conflict`), propagating
   `dependency_deferred` to their dependents while allowing independent tasks to
   compose cleanly unless `--strict` is requested; fatal Git errors fail closed.
5. **Optional isolated `accept.cmd` verification (`--verify-accept`):** Before
   recording a composed candidate, `compose --verify-accept` checks out the
   composed commit into an ephemeral detached worktree with a hermetic
   environment (no user credentials), runs each included task's `accept.cmd`
   array directly without a shell, records `accept_verification` digests, and
   fails closed before ref creation if any acceptance command fails.
6. **Content-addressed receipt, compare-and-swap refs, and verified `release/*`
   branch publication:** `compose` / `orchestrate` writes an fsynced
   `susi-distillation-receipt-v1` JSON record (`receipt_sha256`) under
   `.git/susi-distill/receipts/<name>.json` and creates `refs/heads/distill/<name>`
   via atomic zero-old-OID `git update-ref`. With `--release-candidate`,
   `local-distill` wraps the composed commit `P` in a deterministic single-parent
   envelope commit adding only `releases/distill/<name>.json` at the tip of
   `refs/heads/release/<name>`. `publish-branch <name>` (or `orchestrate
   --verify-accept --release-candidate --publish-release-branch`) pushes
   `refs/heads/release/<name>` non-force through `.githooks/pre-push` and
   `push-policy`, which admits a `refs/heads/release/<name>` update only when it
   is non-deleting, fast-forward/create-only, and carries either a full
   `releases/qualification/<tag>.json` record or a tamper-evident
   `accept_verified_candidate` distillation receipt with zero rejected tasks.
   `publish <name>` continues to push immutable `refs/tags/susi-distill/<name>`
   snapshots.

## Distilling onto a distilled release

A release tag cut by distillation (for example `v0.21.55`) is not on main: it
holds new composed commits whose messages carry `Distilled-Task: <id>` and
`Source-Commits: <oid>,...`. The next release distils onto that tag, and its
range back to main still lists the original source commits. `plan`, `compose`
and `orchestrate` read every distilled commit in `<from>..<base>` (one
`git log` over the history the base has and the stream lacks), drop the
stream commits named in `Source-Commits`, and report a task whose remaining
commits are none as `satisfied_in_base` with reason
`already_distilled_in_base`. Later commits of the same task still apply.
Without this, applying a source commit a second time conflicts with
release-only edits such as the version bump (seen for `v0.21.56` onto
`v0.21.55`: `T-CLAUDERECURSE-1` on `crates/susi-cli-defs/src/os.rs`).

A `Task:` trailer may name several ids separated by commas or whitespace
(`Task: T-A-1, T-B-1`), as `distill lint` reads it; each id is attributed.
A commit attributed to several tasks is still audited against each task's own
scopes, so a commit that closes two tasks' records is out of scope for each
one on its own.

## Evidence on distilled branches

Composed commits are new commits on the release base, so development commits
are not their ancestors. Evidence that binds itself to the commit its check
ran on (the GMCP records under `.agents/gmcp/evidence`, checked by
`tests/gmcp_requirements.rs`) follows the composed commits' provenance: a
`tested_commit` counts as in HEAD's history when it is an ancestor of HEAD or
of a commit named in a `Source-Commits:` line of a commit that also carries
`Distilled-Task:`. The evidence's files must still be byte-identical at the
tested commit and at HEAD, so stale evidence fails on a release branch exactly
as it does on main.

## Evidence bound to a commit is re-recorded, not carried

Some committed records describe the exact tree they were made on, such as the
GMCP vector evidence in `.agents/gmcp/evidence/*.jsonl` (each record names its
commit and the requirements-ledger digest). They are append-only, so
`.gitattributes` unions them (`merge=union`) and distillation can compose them
onto an older base. But development-main records describe main's trees: carried
into a candidate they become the newest records and fail its operations checks.
On a release branch, reset those logs to the base's version and record fresh
evidence for the candidate (`scripts/gmcp-record-evidence.sh`, then `--mastery`)
before `release-qualify`. Main keeps every record. The first distilled
candidate, `v0.21.55` (`T-CLAUDEDISTILL-1`), followed this and records its gate
results in `.agents/evidence/distill-v0-21-55-rc2-T-CLAUDEDISTILL-1.json`.

## What remains separate

Distillation produces an **unqualified candidate** (`qualification_status:
"unqualified_candidate"`), not a production release. Exact-tree Windows x86_64
and Linux x86_64 release qualification gates, license and binary audits,
artifact signing, semantic conflict rewriting across overlapping edits, and
immutable `v*` release-tag promotion remain separate steps before production use.

## Verification

Run the focused unit, push-policy, and bare-Git hook integration suites:

```text
cargo test --offline --locked -p xtask --bin local-distill --test native_push_policy --test git_publisher_native_hook
```
