# Harness coordination across hosts: the repository is the shared state

CEO direction, 2026-11-07: any number of agents, on any host (Windows, Linux,
macOS; a laptop, a cloud sandbox), must coordinate through the susi harness
workflow, and the GitHub repository is the shared medium. No shared filesystem,
no GitHub Actions, no third-party coordinator. These are the technical
decisions.

## What is wrong today

`cargo local-workflow` keeps claims, verification slots, receipts and the
integration pointer under one host's `.git/susi-local-workflow`. A second host
cannot see a reservation, cannot run the gate against the same integration
history, and so pushes to `main` directly. On 2026-11-07 one cloud agent pushed
about 400 commits that way; `main` did not compile for an hour and a half, the
other host's gated ships lost every race, and both agents repaired the same
breakage twice.

## Decisions

1. **Claims are refs on `origin`.** `refs/susi/claims/<agent>` holds the claim
   (scopes, base commit, host, platform triple, created time, expiry). `start`
   creates it with a compare-and-swap push (`--force-with-lease` expecting the
   ref absent); `extend`/`shrink` replace it the same way; `release` deletes
   it. Before claiming, `start` fetches `refs/susi/claims/*` and checks
   overlaps exactly as it checks local claims now. `list` shows every host's
   claims. The compare-and-swap push is the lock; there is no other.
2. **The agent's source is its pushed branch.** `commit` pushes
   `refs/heads/local-agent/<agent>` after the scope guard; the branch is the
   hand-off between hosts.
3. **Ship runs on any host that can build its platform, against `origin`.**
   `ship <agent>` fetches `origin/main` and the agent branch, merges the
   candidate, runs the gate locally (format, licenses, clippy, affected tests,
   task acceptance), then publishes atomically: the receipt goes to
   `refs/susi/receipts/<merge-sha>` and `main` fast-forwards with
   `--force-with-lease=main:<the main it merged onto>`. If `main` moved
   meanwhile, ship re-merges onto the new head and re-runs only the narrowed
   gate for the delta before publishing again; it never force-pushes.
4. **Receipts carry the platform.** `refs/susi/integration/<triple>` is the
   last commit gated on that platform. A ship on Linux advances `main` and the
   Linux pointer; the next Windows ship gates everything since the Windows
   pointer, so a `#[cfg(windows)]` break surfaces at the next Windows ship,
   recorded, instead of by hand. Qualification for release needs the pointers
   of every shipped platform at the same commit.
5. **A gated host can integrate another agent's branch.** `ship --adopt
   <branch>` takes a pushed `local-agent/*` branch whose claim is on `origin`
   and runs it through this host's gate. A host that cannot run the gate (no
   toolchain for the target, no time budget) pushes its branch and stops;
   it never pushes `main`.
6. **Ungated history is tolerated and named.** When `main` advanced without a
   receipt, ship measures the agent's scope from what its branch shares with
   `main` (shipped 2026-11-07, 9b9515c8a) and the receipt lists the ungated
   range it absorbed. Nothing refuses to build; the record shows who skipped
   the gate.
7. **Task records stay in the tree** (`.agents/tasks/T-*.json`), as today;
   the acceptance plan reads them from the candidate.

## What stays per host

Verification slots and warm targets (`slots/<n>`), the host gate lock and the
test-duration history are per-host caches. They never decide anything.

## Order of work (each a gated ship, Claude's lane)

1. Claims on `origin` refs with compare-and-swap; `list` shows all hosts.
2. `commit` pushes the agent branch; `ship --adopt`.
3. Receipts and per-platform integration pointers on `origin`; fast-forward
   with lease and the narrowed re-gate on a lost race.
4. Retire `.git/susi-local-workflow/{claims,integration,publish}`.

Measured goal: a cloud agent on Linux and this laptop on Windows each run
`start`, `commit`, `ship` from their own clone with nothing shared but
`origin`, and neither can push `main` without a receipt.
