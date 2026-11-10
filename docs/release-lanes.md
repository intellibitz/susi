# Release lanes: many agents, one release, no conflicts

`release-lanes` implements CEO Mandate 74 section 3 (`docs/ceo-mandate-74.md`).
Development `main` is an ungated feature dump. A release is distilled from it,
and several agents move the same release forward in parallel. GitHub never
merges and never shows a pull request: every move is a Git operation pushed as
a normal fast-forward. The tool uses Rust and the installed `git` only, and works
in any checkout, including `susi-commercial-apps`.

## Commands

```text
cargo build --locked -p xtask --bin release-lanes
release-lanes init    --name 2026-11-12-rc1 --base <rev> --from origin/main --select T-A-1,T-B-1
release-lanes status  --name 2026-11-12-rc1 [--json]
release-lanes claim   --name 2026-11-12-rc1 --lane lane-01 [--stale-secs 7200]
release-lanes ship    --name 2026-11-12-rc1 --lane lane-01 --cmd cargo test --locked --workspace
```

`ship` runs the three steps that can also be called one at a time:

```text
release-lanes pick    --name <release> --lane <lane> [--resume <rev>]
release-lanes verify  --name <release> --lane <lane> [--here] [--keep] --cmd <program> [args...]
release-lanes advance --name <release> --lane <lane>
release-lanes unclaim --name <release> --lane <lane>
```

Every command takes `--remote <name>` (default `origin`) and `--agent <id>`
(default `$SUSI_AGENT`, then `git config user.name`). `init` accepts the
`local-distill plan` selectors (`--from`, `--select`, `--tasks-from-range`,
`--feature`, `--plan-manifest`), or a saved receipt with `--plan <file>`, and
supports `--dry-run`.

## Where the state lives

| What | Where | Rule |
| --- | --- | --- |
| Release line | `refs/candidates/release/<name>` | fast-forward only; never deleted (`push-policy`) |
| Manifest | `.agents/releases/<name>.json` on the release line | `susi-release-lanes-v1` |
| Lane claim | `refs/claims/release-lanes/<name>/<lane>` | JSON blob, atomic create-if-absent |
| Local candidate | `refs/susi/lanes/<name>/<lane>` | local only, never pushed |
| Evidence | `.git/susi-lanes/<name>/<lane>.evidence.json` | `susi-lane-evidence-v1`, bound to one tree |

The release line is not a branch, so a Git host does not list it, does not
offer a pull request for it and shows no merge button. The final immutable
`vX.Y.Z` tag is still cut by `local-release qualify`, which `push-policy` requires.

## How it stays conflict-free

1. **Lanes.** `init` runs the distill planner (dependency closure, cycle and
   scope checks), then groups tasks: two tasks share a lane when their paths
   overlap (`crates/a` and `crates/a/src/x.rs` overlap; `crates/a` and
   `crates/ab` do not) or one depends on the other. Lanes are therefore disjoint
   in files. Tasks keep their dependency order inside a lane.
2. **Claims.** One agent per lane. Git refuses to replace an existing non-commit
   ref without force, and the server compares the old value atomically, so two
   agents claiming the same lane cannot both win. `--stale-secs` takes over a
   claim only when it is older than the given age.
3. **Picks.** In memory, without touching any worktree: `git merge-tree
   --write-tree --merge-base=<source parent> <tip> <source>` and `commit-tree`,
   keeping the original author and adding the `(cherry picked from commit ...)`
   line of `git cherry-pick -x`. Commits already present (by that line or by
   `git patch-id --stable`) are skipped. A conflict stops with the file list.
   Resolve it with a real `git cherry-pick -x` in your worktree, then
   `pick --resume HEAD`.
4. **Exact-tree evidence.** `verify` checks out the exact candidate in a fresh
   temporary worktree (or `--here` in a clean checkout of exactly that commit),
   runs the command and records the tree, command, exit code and duration.
   `advance` refuses evidence for any other tree, and refuses failed evidence.
5. **Atomic advance.** `advance` adds the manifest update on top of the
   qualified code, then one `git push --atomic` moves the release line by a
   fast-forward and deletes the claim. If another agent advanced first, the
   candidate no longer descends from the tip. The tool refuses it with
   `lost race`, and nothing changes. `ship` then re-picks on the new tip and
   re-qualifies, up to `--attempts` (default 3). Nothing is ever forced.

## Verified

- `cargo test --locked -p xtask --bin release-lanes release_lanes::`: 6 tests on
  throwaway repositories. They cover lane grouping, ref-name validation, two
  agents racing one release (the loser re-picks and lands), exact-tree evidence,
  conflict reporting with resume, stale-claim takeover and create-only releases.
- End to end on real Susi history (2026-11-11). Three tasks from `main` were
  planned by the real `local-distill`, split into two lanes and shipped by two
  agent identities through the repository's real `push-policy` hook into a
  scratch bare remote. One agent lost the race and landed on retry. The finished
  release had exactly the selected files, matching `main`, plus the manifest,
  and both claims were gone.

## Not covered (open work)

- Evidence is never reused across trees. Speculative stacks (qualifying
  tip + A + B before A lands) are not automated.
- Claims have no heartbeat. Staleness is age since claiming, decided by the
  agent that passes `--stale-secs`.
- `init` plans once. Adding tasks to an open release means a new release, or a
  manual manifest change through a scoped commit on the release line.
- A conflict needs a human-style resolution by an agent. The tool reports it
  but does not resolve it.
