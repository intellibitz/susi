# Parallel development without weakening landing evidence

Status: **Windows-verified development diagnostics**, currently on
`local-agent/dot-parallel-capacity`; publication is tracked by its task record.
Its cooperative stock-toolchain development diagnostics are not a replacement
for the native
integration gate, a production qualification, or approval to start 100 builds.

## Three separate loops

- **Develop independently.** Reserve disjoint scopes with native
  `local-workflow start`, keep one branch/worktree per agent, and use targeted
  feedback while editing. Source editing and review do not consume build tokens.
- **Publish unqualified development.** The latest CEO direction removes mandatory
  gates from development main. Agents submit labelled source through the safe
  Git publisher, preserving concurrent commits with non-overwriting ref updates.
  Targeted diagnostics remain useful feedback, not required main-admission proof.
  This change does not itself replace the existing publisher, bypass an enforced
  guard, alter branch security or certify main. The publisher owns that transition.
- **Qualify releases.** Keep the complete native production qualification,
  both supported release platforms, artifact identity, health and rollback.

## Native targeted feedback

The existing native `susi-dev` executable now accepts:

```text
susi-dev dev capacity 100
susi-dev dev check susi-harness
susi-dev dev clippy susi-harness --jobs 2 --wait-ms 2000
susi-dev dev test susi-harness --jobs 2
```

Use an updated, verified native `susi-dev` executable built in private launcher
output. This change does not install or replace the production executable. A
Cargo build of the launcher itself must also have private output and obey the
current host build coordinator. There is no new binary, dependency or shell
runtime in the product path.

`check` and `clippy` include all package targets. `test` executes the package's
normal Cargo tests including doctests. Selecting `xtask` includes
`release-tooling`. All commands pin the current native target, `--offline`,
`--locked`, the private worktree target and an explicit job count. Checks are
package-targeted feedback, not automatic reverse-dependency acceptance. Use the
existing affected-package checks when useful, and mandatory full native
qualification before a release.

The command refuses the primary checkout, an unclaimed linked worktree, and a
branch other than the matching native `local-agent/<worker>` branch. It shares
`target/check-gate/gate.lock` with this worktree's existing native check, reuses
this worktree's stable `target` cache, and creates fresh runtime/home/temp state
for every attempt. It does not share mutable build outputs with another agent.
The child environment is allowlisted; provider credentials are not inherited.
These fast checks deliberately use the pinned installed stock toolchain, exactly
as ordinary targeted Cargo development checks do. They do not prepare or admit
a custom production sysroot, do not build release artifacts, and do not claim
license or artifact qualification. Every result explicitly says stock-toolchain
diagnostic and carries no landing/release receipt. The existing native check,
legacy ship and release paths are unchanged by this implementation. Release
qualification still requires trusted sysroot and all mandatory checks. This is a
development/qualification separation,
not a relaxed production check.

The launcher must be installed outside this worktree's mutable target output, or
in its existing private `target/local-tools` area. It is never renamed or removed
by admission, including a busy result. Cargo rebuilds cannot replace a running
Windows executable, so an unsafe launcher location is rejected before launch.

## Capacity and retry semantics

Coordination lives under canonical `CARGO_HOME/susi-dev-capacity-v1`, resolved
before creating private runtime homes. The printed coordinator path identifies
the coordination domain. Agents must use the same canonical Cargo home to share
this budget. Multiple Cargo homes, users or machines are separate domains.

The initial ceiling is one fewer logical CPU, at least one and at most 16.
The persisted ceiling cannot silently grow when another process observes a
different CPU affinity. Each command reserves its explicit Cargo job count and
bounds the normal Rust test, nextest and Rayon thread counts to that count.
These are cooperative CPU scheduling limits, **not** a memory limit or an OS
sandbox against tests spawning their own processes.

Admission is all-or-nothing. No caller sleeps while holding a partial allocation.
The default wait is at most two seconds; `--wait-ms` permits 0 through 60000.
Exit 75 means no check ran: continue independent source/review work, then retry.
It is never a successful check and must not close a task. The capacity command
reports an ideal equal-work scheduling model, not a timing prediction or a
fairness guarantee. Agents that need only one job can explicitly request one.

Token lock files are permanent; never delete them to "fix" contention. OS locks
release if a holder dies before launching a child. Before launching builds, the
supervisor also persists running markers. A killed supervisor may leave Cargo
alive, so those markers keep capacity unavailable until normal completion or
independent process-tree verification. An interrupted worktree also retains
`target/check-gate/dev-running`. Do not clear these markers, start a legacy
check in that worktree or delete lock files merely because a PID looks stale.
Recovery must establish that the owned Cargo process and descendants have ended
before releasing its markers; no automatic recovery command is claimed here.

## Existing gate coordination is still an open prerequisite

The current `local-workflow` gate does not acquire these capacity tokens.
Its live-marker registry derives from HOME/LOCALAPPDATA, so existing private-home
launchers can split the registry. Its instantaneous share calculation has a
minimum of two jobs and cannot revoke resources from already running commands.
At 64 simultaneous slots that minimum permits at least 128 requested jobs,
even on this 8-logical-CPU desktop. Sharing a counter alone would not fix the
stale-allocation race.

Consequently, continue the externally coordinated single-heavy-build rule
while legacy gates run. This command cannot enforce a host-wide total against
unmodified ships or raw Cargo invocations. It is **not yet safe to declare the
100-agent workflow ready**.

Any retained heavy verification path in the separately owned
`xtask/src/bin/local-workflow.rs` needs a reviewed change to
use the same stable coordinator, reserve resources around every heavy command,
hold the reservation through real child completion, retain failure/cancellation
evidence, and keep private targets. Verification must remain parallel within
that budget; publication alone should have a short serialized critical section.
For `integrate_many_with`, moving verification out of its existing integration
lock also needs an exact-base compare-and-swap and a re-gate of the actual
combined tree after a competing landing. Neither change may reuse stale proof
or activate the unfinished workspace runner.

## Measurements and verification limits

2026-11-10, DESKTOP-53DT8GM: Windows x86_64, 8 logical CPUs, 68,492,570,624 bytes
physical memory. Final native command suite: 25 passed, zero failed/ignored,
2.79 seconds executing tests, 11.480 seconds including the warm rebuild. Strict
Clippy passed in 2.938 seconds and the executable build passed in 3.158 seconds.
Independent source review found no remaining blocker for this diagnostic scope.

Real `dev test susi-build-policy --jobs 2` ran five unit tests and the actual
successful zero-doctest command twice. Outer wall clock was 3.084 seconds for
its first native-target build and 1.512 seconds warm. A held native worktree
check lock produced retryable exit 75 after a measured 77 ms wait (402 ms outer
time), without changing the stable executable or leaving a running marker.
These are small package diagnostics, not whole-workspace or release timings.

Capacity tests cover 100 simultaneous contenders, atomic admission, budget
release, corrupt state, durable interrupted-run reservations, prelaunch partial
write rollback, and actual child termination before launch. With seven tokens
and two jobs/check, the equal-work 100-request model has three concurrent checks
and 34 waves. This is not 100 builds and does not establish a delivery budget.

The earlier 22-test run and one Clippy finding are retained in the task history;
all review corrections are covered by the final result above. Logs and source
hashes are recorded in the task. Linux execution, cross-host coordination,
legacy-gate participation, main publication and full production qualification
are separate verification obligations. No released artifact or full 100-agent
readiness follows from these focused results.
