# Harness coordination across hosts: the repository is the shared state

The [active engineering contract](../AGENTS.md) governs authorization, ownership
and publication. Agents work autonomously within that authorization, using
isolated worktrees, private runtime state and disjoint task scopes. Delegation
keeps explicit contracts, budgets, cycle detection and attenuated permissions;
it never expands the caller's authority.

Git is the portable source and publication transport. Use native commands and
normal history-preserving updates. No hosted review service or provider API is
required. Main is unqualified development; a successful publication does not
qualify an artifact for release.

## Ownership and state

| State | Where it belongs | What it establishes |
|---|---|---|
| Source and active task intent | Owned worktree and committed candidate | Proposed behavior, scope and acceptance command |
| Native reservations | The clone's common Git directory, under `susi-local-workflow/claims` | Cooperative ownership within that coordinator |
| Native integration head and receipt | Coordinator-managed Git refs and local receipt storage | The exact candidate and checks accepted by that coordinator |
| Published development source | The authorized remote's `refs/heads/main` | Source accepted by that remote, after its actual value is confirmed |
| Agent handoff | `refs/agents/<agent>` | An owned candidate available for another host to inspect and integrate |
| Build targets, verification slots, runtime homes and retained run history | Private local storage outside tracked source | Local working state; never authority or proof merely because it exists |

Native local reservations do not by themselves become a distributed lock.
Before work spans hosts, use the authorized shared coordination surface and
inspect the actual claims it exposes. Do not assume another clone can see a
local reservation, warm target or integration receipt. Coordination messages
are not acceptance evidence. The native board path refuses the protected public
source repository; preserve that refusal and the configured identity resolution.
External coordination messages require authorization under the active contract.

A task has one live owner while work is active. Keep its scope and progress
current, and release ownership when work is handed off, completed or stopped.
Retain a failed candidate and its claim for repair when the native coordinator
requires it. Never remove another worker's reservation, reset its worktree or
replace its runtime state. Worktree isolation is cooperative and does not replace
OS sandboxing or tool authorization.

## Prepare and integrate

Use the [native bootstrap](native-bootstrap.md) and
[mission runbook](harness-runbook.md) for executable entry points. The harness
adapter reserves an isolated worktree, checks task ownership and scopes, runs
acceptance and commits the candidate. Its native integration path is
`local workflow integrate <claim>`; a wave may integrate several disjoint claims
in one combined candidate. Landing is serialized even when preparation runs in
parallel.

The command's actual acceptance, source, license and integration checks must
pass. A failure preserves work for repair; it is not permission to skip a check,
loosen a test or substitute a different artifact. Local integration success
must be distinguished from remote publication and from production qualification.
The harness supervisor's `done` result follows native local integration; inspect
the remote receipt before claiming publication.

## Publish and reconcile

Fetch and merge current remote main before starting, when resuming and before
committing if reachable. Review meaningful, ready work for integration about
every ten minutes. This is a cadence against drift, not a compulsory cooldown or
a reason to publish incomplete work. Capacity and serialized publication govern
parallelism.

An authorized agent may publish a normal fast-forward to unqualified main, or
hand off its candidate through its own `refs/agents/<agent>` ref. Use
[agent refs](agent-refs.md) and the
[storage publisher](git-storage-publisher.md); never force-push, rewrite shared
history or move another agent's ref. Local `local-agent/<agent>` branches remain
local. Protected coordinator refs, qualified release streams and immutable
release tags keep their separate rules.

The harness retains both branch-only and fast-main integration policies.
`IntegrationPolicy` checks the selected mode, owned identity, task trailers,
scopes and protected refs. Follow the actual policy selected by a caller; the
availability of another authorized publication path does not bypass a refusal.
Main has no universal mandatory pre-push qualification gate, while a native
integration or release command still enforces its own checks.

Workflow reconciliation requires an admitted `workflow_harness_v1` MCP provider;
storage auditing retains a standalone fallback after definitive provider absence.
Confirm the accepted remote head and receipt;
a pushed candidate, local merge or retained historical record is insufficient.
If another publisher advances main, reconcile from the observed source through
the normal workflow. After an uncertain mutation response, inspect the remote
and retained receipt before retrying: a lost reply must not replay an operation.
An accepted publication with a cleanup warning remains accepted; repair the
reported local archival problem without resubmitting the original mutation.

## Records and release qualification

Active task intent remains available until acceptance and publication settle.
The workflow receiver archives accepted native task records in local Git metadata
and removes their active source copies in a separately confirmed cleanup commit.
Legacy records remain readable. Missing history never supplies completion proof;
a cleanup warning cannot stand in for successful archival. Do not add activity,
incident or measurement histories to source. See [record storage](evidence.md#record-storage).

Production qualification binds the exact combined source, dependencies,
toolchain, target, features, artifact hashes and passing checks. Each shipped
platform needs its own evidence; one host's result cannot qualify another
platform. Preserve licenses, required attribution, signing authority, health
checks and a verified rollback path. Published release tags remain immutable.
See [qualification](evidence.md#production-qualification) and
[gate receipts](gate-receipts.md).

These procedures do not claim the entire agent engine has moved out of core,
that every persistent compatibility identity is generic, or that the core-size
goal is met. Further implementation transfers require a working destination and
actual socket parity before removal; authorization and provenance stay live.
