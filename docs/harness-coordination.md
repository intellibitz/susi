# Harness coordination across hosts: the repository is the shared state

## Current CEO direction, 2026-11-10

Read [Mandate 73](ceo-agent-mandate.md) first. Three isolated laptops run dot,
opus, carbon and grok, with an available authorized driver on each laptop: dot,
duet, another agent or a capability-verified Susi harness. Driver/engine recursion
applies to every container component with explicit contracts, budgets, cycle
detection and no delegated permission expansion. Cloud
sessions are independent and autonomous within user authorization. Susi's master
brain owns distillation, task/feature manifests, branch composition and serialized
integration/publication. Main is unqualified development with no mandatory push
gates. Full exact-tree gates apply to release branches and snapshots. Main
contributions use normal fast-forward updates, with no fixed push cadence or arbitrary
agent-count limit; measured host capacity and serialized publication govern.
Git is portable storage, not a GitHub API or hosted-workflow dependency. Branches
may advance; published release tags are immutable. Access controls, private
commercial boundaries, production gates and rollback remain unchanged.

The older decisions and incident notes below describe history, design and older
implementation. They are not proof that distributed claims, adoption or receipts
are deployed. In particular, fixed gate-cycle cooldowns and a single-laptop-only
publication role and mandatory main-push gates are superseded. Existing `IntegrationPolicy` main-gate restrictions must
be migrated with honest validation. Authorized normal history-preserving main
publication may proceed while that tooling is updated. Never bypass live access
or security controls, weaken release gates, or change repository permissions. Keep incident
safeguards, release exact-tree gates and honest origin evidence.

## Historical design and implementation notes

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
2. **The agent's source is its pushed agent ref.** `commit` pushes the local
   `local-agent/<agent>` branch to `refs/agents/<agent>` after the scope
   guard; that ref is the hand-off between hosts. It is not a branch on the
   Git host, so no pull request can be opened against it (CEO decision
   2026-11-11; `docs/agent-refs.md`). Pushing `refs/heads/local-agent/*` is
   refused.
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
   <branch>` takes a pushed `refs/agents/<agent>` ref whose claim is on `origin`
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

## Remote-agent mandate (CEO, 2026-11-08) and where the harness enforces it

The night of 2026-11-07 showed what the design above has to make impossible:
a remote agent pushed about sixty ungated commits to `origin/main`, deleted
roughly 20,000 lines as "migrated to commercial" with nothing on the
commercial origin, broke main's tests twice, left four capabilities as error
stubs, and cost two gated landings seven attempts. The mandate (workspace
`AGENTS.md`, "Remote and Cloud Agent Workflow") has eight rules; the harness
makes them mechanical in `susi_harness::workflow::IntegrationPolicy`:

| rule | harness mechanism |
|------|-------------------|
| one `refs/agents/<agent>` ref per agent (published from the local `local-agent/<agent>` branch), never `main`, never `refs/susi/*`, never a `refs/heads/local-agent/*` branch | `publish_branch`, `publish_ref`, `check_push_target`: a wave is planned only with each task's branch; a push target that is `main`, a coordinator ref, another agent's ref, a retired `local-agent/*` branch (`RetiredAgentBranch`) or anything else is a `PolicyViolation` |
| `Task: T-<AGENT>-<n>` trailer on every commit, record scopes equal the claim | `check_commit_trailers` over the batch; the acceptance plan already refuses a record whose scopes differ |
| destination first; no stubbed ports | `check_migration(MigrationProof)`: no commercial commit, not declared, not compiling, not tested, or a kept port with no plug, and core may not delete |
| verify before every push | the brief lines list the exact commands; the gate runs them again on adoption |
| one push per gated cycle; read the board | `gate_cycle_secs` in the brief; the stop note is read from the board before each wave |
| never another agent's claim | `next_wave` already refuses overlapping scopes; `validate_plan` refuses two tasks that would publish on one branch |
| report only what the origin shows | receipts and integration pointers on `origin` (decisions above) are the only "landed" |

`brief_lines(agent)` is prepended to every worker prompt the executor hands
out, so a remote worker reads the rules with its task. The native
coordinator's `ship --adopt` (order of work, step 2) calls
`check_commit_trailers` and refuses a branch that fails it; the GitHub
ruleset on both repositories keeps `main` behind the gate identity (a machine
account per remote agent, none on the bypass list: CEO action).

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
