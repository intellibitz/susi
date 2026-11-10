# Release distillation: plan-only planner (first slice)

Susi's intended master brain picks labelled contributions out of development
main and composes feature and release branches from them. This document covers
the **first slice only: a planner that decides what a selection would contain.
It composes nothing.** Task `T-GROKDISTILL-1`. It is built to feed the release
work from task records (workspace board item 113, step 2, owned by Dot), not to
replace it.

## What it does

```text
cargo run --offline --locked -p xtask --bin distill -- plan --tasks T-A-1 T-B-1 [--base <rev>] [--out plan.json] [--json] [--isolate]
cargo run --offline --locked -p xtask --bin distill -- plan --tasks-from-range v0.21.52..v0.21.53 [--out plan.json]
```

1. It resolves `--base` (default `HEAD`; with `--tasks-from-range` the default
   is the range end) to a commit. It reads every `.agents/tasks/*.json`,
   `.agents/tasks/done/*.json` and `.agents/tasks/in-progress/*.json` **from
   that commit through `git ls-tree` and `git cat-file --batch`**. The working
   tree, index and uncommitted records are never read.
2. It takes the dependency closure of the requested ids through `deps` and
   the legacy `dependencies` field.
3. It gives every task in the closure one verdict:
   - **rejected** for an `unknown_id`, an `ambiguous_record` (two different
     records for one id in the same directory), a `dependency_cycle`, an
     `abandoned_status` (failed, canceled, retired), a `missing_dependency`
     (it depends on an unknown id) or a `dependency_rejected`;
   - **deferred** for an `open_status` (in progress, pending, no status),
     `no_provenance_in_base`, `missing_evidence` (an `evidence` path absent at
     base), `blocked_by_dependency` (a dependency is deferred) or, with
     `--isolate`, a `scope_conflict`;
   - **included** otherwise.
   A record in `done/`, or one with a `closed` object, is closed. The status
   text is mapped as described in `.agents/schemas/task.schema.json`.
4. **Provenance:** a closed or implemented task needs a commit in base
   history. That is either its `closed.commit` verified with
   `git merge-base --is-ancestor`, or a commit whose message has a
   `Task: <id>` trailer.
5. **Scope overlaps:**
   - `unordered_overlap`: two included tasks share a path and neither depends
     on the other. Base history orders them, so this is informational.
   - `entangled_unselected`: an included task shares a path with a task
     outside the selection that is already in base. Composing the selection
     without the rest of base would need conflict resolution. `--isolate`
     defers such tasks.
   - A task without a scope is noted, and the plan warns, because its
     overlaps cannot be checked.
6. With `--tasks-from-range A..B` the requested ids are the `Task:` trailers
   of the non-merge commits in `A..B`. A `range_comparison` section then
   reports:
   - commits without a task trailer;
   - range tasks that were not included, with the reasons;
   - closure tasks from outside the range;
   - files changed in the range that no included task's scope covers.

Output: a plain-text manifest on stdout (`--json` prints the JSON instead),
and the JSON manifest in `--out`. Its schema is
`.agents/schemas/feature.schema.json` (`susi-distill-plan/v1`), and a unit
test keeps the two in sync. The output has no timestamps and is sorted
throughout, so the same base and selection give byte-identical output.

**Bounds:**
- 50,000 records;
- 1 MiB per record;
- 256 MiB per Git output;
- 10,000 requested ids.

Only local Git commands run: `rev-parse`, `ls-tree`, `cat-file`, `log`,
`merge-base` and `diff`. `GIT_NO_REPLACE_OBJECTS=1` and `GIT_OPTIONAL_LOCKS=0`
are set, and there is no network access. The only file written is `--out`.
No dependency was added: the planner uses the standard library and the
workspace's existing `serde_json` facade.

## First real result: v0.21.53 cannot yet be reproduced from task records

`plan --tasks-from-range v0.21.52..v0.21.53` (base `afdbc3a99`, the v0.21.53
tag) on 2026-11-11 gave:

- 80 non-merge commits, of which **61 have no `Task:` trailer**;
- 15 task ids named by the other 19;
- **0 included, 15 deferred, 0 rejected.** Every one of the 15 records was
  still `in_progress` (or had no status) at the tag. Two more were also
  blocked through their deps (`T-PEERCHAN-1` by `T-CORENET-1`,
  `T-PEERSTATUS-1` by `T-HOMECLUSTER-1`). The same ids planned against
  origin/main `57dfb2e78` are still all deferred, because the records were
  never closed after shipping;
- 248 files changed. Even if all 15 tasks were counted, their scopes cover
  only 191 of them. 57 files have no task scope; 33 of those are under
  `.agents/`.

So the release content is not recoverable from task records today. Reasons:
- shipped tasks are left open;
- most commits carry no task label;
- scopes do not cover the changed files.

This matches item 113's condition 2. Labelling at the time of the change and
closing records when they land are the prerequisites for distillation.

## What is missing (not claimed)

This is **not** a distillation service. Not implemented:

- composing a branch: merging or cherry-picking the included tasks' commits
  onto a release branch;
- resolving conflicts between entangled or overlapping tasks;
- running each `accept.cmd` and the standard gate on the composed tree;
- qualification evidence for that exact tree: toolchain, platforms, licences,
  smoke tests and artifact hashes;
- receipts, `releases/<tag>.json`, tag publication, promotion and rollback;
- choosing a selection automatically (the planner only evaluates one it is
  given);
- partial or commit-level selection of a task;
- checking that a task's commits touch only its declared scope.

Treat a plan as an input to those steps, never as evidence that a release
builds, passes or qualifies.

## Verification

```text
cargo test --offline --locked -p xtask --bin distill
cargo clippy --offline --locked -p xtask --bin distill --all-targets -- -D warnings
cargo fmt -p xtask --check
```

Twelve unit tests cover:
- transitive closure;
- cycles, including a self-dependency, and the dependents they reject;
- unknown ids and missing deps;
- open work deferring its dependents, and abandoned work;
- provenance and evidence requirements;
- overlap reporting and `--isolate`;
- determinism under reordered input and repeated ids;
- duplicate and ambiguous records;
- every historical record shape;
- strict argument parsing;
- schema agreement;
- a real Git fixture: it reads the revision rather than a modified working
  tree, compares a range, gives a byte-identical repeat run, and changes no
  ref, index or tree.
