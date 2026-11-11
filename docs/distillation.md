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

## Labelling rules and `distill lint` (T-GROKLABEL-1)

A release can only be assembled from records that say what landed. Three
rules make development main distillable:

1. **Every commit carries `Task: <id>`** (`T-<AGENT>-<n>`; several ids may be
   comma- or space-separated, or given as repeated trailers). Place `Task:` and
   every companion trailer (`Feature:`, `Depends:`, and for cloud-harvested raw
   features under [CEO Mandate 75](ceo-mandate-75.md): `Harvest: H-<AGENT>-<n>`,
   `Origin: <url>@<sha>:<path>`, `Origin-License: Apache-2.0`) in the **final
   paragraph** of the commit message with zero blank lines between trailers so
   Git trailer parsing sees all of them. The native launcher's `Task: LOCAL-<name>`
   is accepted during migration but has no task record, so it cannot be
   distilled; prefer a real task id.
2. **Every task declares `scopes`** (repo-relative files or directories, the
   same scopes it claims) and its commits stay inside them. The task's own
   record and the evidence files it lists are always in scope.
3. **Close the record when its work lands** (`susi-commercial tasks close`),
   so a landed task is not left `in_progress`.

`distill lint` checks these over a commit range, deterministically and without
changing anything:

```text
cargo run --offline --locked -p xtask --bin distill -- lint --range origin/main..HEAD [--strict]
cargo run --offline --locked -p xtask --bin distill -- lint --range v0.21.53..origin/main [--json] [--out report.json]
cargo run --offline --locked -p xtask --bin distill -- lint --range origin/main            # whole history
```

- One `git log --no-merges --name-only` pass over the range, one record read
  at `--base` (default: the range end) through Git, and one batch read of the
  local claim refs. The working tree is never read; nothing is written except
  `--out`. Bounded at 50,000 commits. Merge commits are not linted (they carry
  no authored change).
- It reports commits without a `Task:` trailer, commits labelled only
  `LOCAL-<name>`, malformed trailer values, trailers naming a task with no
  record, tasks whose commits landed while the record is still open
  (`in_progress`, `open`, or no status in `.agents/tasks/`) or only
  `implemented`, tasks with no scopes, and commits touching files outside
  their tasks' scopes. A scope covers itself, everything under it, or (for a
  glob) everything starting with its literal prefix.
- Scopes come from the record; when the record has none they come from the
  task's claim ref `refs/claims/<id>` if the local clone has it.
- Default exit is success with a report. `--strict` exits non-zero when there
  is any finding; it is an optional fast check for an author's own range, not a
  gate on main (Mandate 73: main has no mandatory pre-push gate).

**Run it before publishing your own work:** `distill lint --range
origin/main..HEAD --strict` takes well under a second. The publisher
(`local-snapshot submit`, Dot's `T-DOTGITPUBLISHER-1`) is deliberately not
changed: its receipt and output are part of its tested contract, and a lint
warning there would not change what it publishes. The rule is documented and
the check is one command away instead.

**Fixed: `tasks close` keeps every field.** Close in Susi Commercial Apps
(`crates/susi-dev-workflow/src/admin/tasks.rs`) used to drop `scopes`,
`results`, `limitations`, `provenance` and every other field its `Task` struct
did not name. Commercial commit `4d9f344` keeps them with a
`#[serde(flatten)]` map and adds the round-trip test
`close_keeps_every_labelling_field_of_the_record`. The claim-ref fallback for
scopes in `distill lint` stays for older records closed before the fix.

The first full report is `.agents/evidence/labels-T-GROKLABEL-1.json`: counts
and the list of tasks whose work is on main while the record is still open,
for each owner to reconcile. Records were not mass-edited.

## Merges happen in Susi: host-created commits (T-FURY-1)

The CEO's rule (2026-11-11): the Git host is storage only, and every merge
happens in Susi, governed by the agents or the harness. Susi's own paths are
`local-snapshot submit` for development main, `local-distill` for release
branches and `release-qualify` for tags; each commits with the agent's own
identity. A commit object that the hosting service wrote itself (a web
pull-request merge, a squash or rebase merge, a web edit, or a commit made
through the host's REST or GraphQL API) instead records the host's service
identity as its committer, `noreply@github.com` on the current host. That is
how such work is recognised; the tools never contact the host.

- `distill lint` reports every such commit in the range, merges included
  (`commits_created_by_git_host`, `merges_created_by_git_host`); each one is a
  finding, so `--strict` fails.
- `distill plan` rejects a task whose provenance commits the host created,
  with reason `merged_outside_susi`, and its dependents with it.
- `local-distill` refuses the same task (`reason_code: merged_outside_susi`),
  and every receipt lists the host-created commits in the composed stream
  (`git_host_created_in_stream`) and the identities checked
  (`git_host_identities`).
- **The way back in** is to re-land the change through Susi. When the content
  is already on main and the distilling agent decides to take it as it is, it
  adopts each commit by its full id: `--adopt-host-commit <full-id>`
  (repeatable; an abbreviation is refused so it can never adopt more than
  intended). The plan records the adoption in the task's notes and warnings;
  the `local-distill` receipt records it in `adopted_host_commits`.
- **Re-landing through Susi (T-FURY-5)** makes the work distillable again
  without trusting the host. For each host-created commit an agent makes a
  replay: a local commit with the host commit's exact change on the same
  parent, the original message and `Task:` trailer, plus
  `Relands: <full host commit id>`. A Susi merge (`-s ours`, since the
  content is already on main; it names the host merges with `Relands:` too)
  joins the replays to main through `local-snapshot submit`. The tools honour
  a re-land only when Git proves it: a replay must be a non-merge commit by a
  non-host committer whose raw change (paths, modes, blob ids) equals the host
  commit's; a re-landed merge must be an ancestor of the commit naming it.
  Then `distill lint` lists it under `host_commits_relanded`, the plan notes
  `relanded_host_commit`, and `local-distill` composes the replay instead of
  the host commit (`relanded_host_commits_in_stream` in the receipt). A claim
  that does not verify is a lint finding (`reland_claims_not_honoured`) and a
  plan warning, and the host commit stays refused.
- Other hosts: `git config --add susi.hostCommitter <email>` adds an identity
  without a code change. A host whose web merges reuse the clicking user's own
  identity cannot be detected this way; on such a host the server-side rule
  (only the gate identity may update `main`) is the control.

What this does not do: it cannot stop the host or another client from writing
to `main`; it makes such writes visible and keeps them out of distilled
releases unless an agent adopts them on record. A pull-request merge whose
branch commits were made locally is a lint finding for the merge; the tasks it
carried stay distillable because their own commits were made by an agent.

First real run (2026-11-11, `v0.21.55..origin/main` at `b91a3070e`): 154
commits, 11 created by the host (2 merges titled "Susi development
submission" that no Susi publisher made, and 9 commits), all
`Antigravity <noreply@github.com>`. `distill plan --tasks-from-range
v0.21.55..origin/main` rejects T-AGYCLOUD-4, -5, -6 and -7 as
`merged_outside_susi`.

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
builds, passes or qualifies. Branch composition, candidate orchestration,
disjoint release lanes, atomic one-version-one-cut version reservations, and
pure-`Apache-2.0` harvest release-gate verification under the
**Raw Source (`main`) -> Distill (`release`)** architecture
([CEO Mandate 75](ceo-mandate-75.md)) live in the companion binaries documented
in [`docs/release-distillation.md`](release-distillation.md),
[`docs/release-lanes.md`](release-lanes.md), [`docs/releases.md`](releases.md),
and [`docs/harvest.md`](harvest.md).

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
