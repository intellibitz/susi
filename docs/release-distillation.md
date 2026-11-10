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
   `git merge-tree --write-tree --merge-base=<commit-parent> <candidate> <commit>`,
   carrying only that task's delta rather than merging intermediate unselected
   commits from `main`. Conflicting tasks are isolated and recorded under
   `deferred` (`merge_conflict`), propagating `dependency_deferred` to their
   dependents while allowing independent tasks to compose cleanly unless
   `--strict` is requested.
5. **Optional isolated `accept.cmd` verification (`--verify-accept`):** Before
   recording a composed candidate, `compose --verify-accept` checks out the
   composed commit into an ephemeral detached worktree with a hermetic
   environment (no user credentials), runs each included task's `accept.cmd`
   array directly without a shell, records `accept_verification` digests, and
   fails closed before ref creation if any acceptance command fails.
6. **Content-addressed receipt and compare-and-swap refs:** `compose` writes an
   fsynced `susi-distillation-receipt-v1` JSON record (`receipt_sha256`) under
   `.git/susi-distill/receipts/<name>.json` and creates `refs/heads/distill/<name>`
   (or `refs/heads/release/<name>` with `--release-candidate`) via atomic
   zero-old-OID `git update-ref`. `publish` pushes create-only snapshot tag
   `refs/tags/susi-distill/<name>` and verifies the remote ref after push.

## What remains separate

Distillation produces an **unqualified candidate** (`qualification_status:
"unqualified_candidate"`), not a production release. Exact-tree Windows x86_64
and Linux x86_64 release qualification gates, license and binary audits,
artifact signing, semantic conflict rewriting across overlapping edits, and
immutable `v*` release-tag promotion remain separate steps before production use.

## Verification

Run the focused unit and bare-Git integration suite:

```text
cargo test --offline --locked -p xtask --bin local-distill
```
