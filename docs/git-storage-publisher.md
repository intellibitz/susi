# Git storage publisher: first portable slice

Main is unqualified development storage. Main admission has no mandatory test
gate; production releases retain full qualification.
This command does not run a test gate before a main submission.
No GitHub API, Actions, server extension or new Rust dependency is introduced.
The publisher uses Rust's standard library and the installed Git tool.
An ordinary configured Git remote is required, not GitHub: a local bare repository,
self-hosted Git or any compatible hosted Git service works. No third-party cloud,
GitHub account, API, Actions, webhook, status check or PR is a runtime prerequisite.

## Submit development source from any agent session

Reserve an isolated worktree and commit scoped work normally. Build both native
tools together once per source update, then submit:

```text
cargo build --offline --locked -p xtask --bin local-snapshot --bin push-policy
cargo run --offline --locked -p xtask --bin local-snapshot -- submit worker-unique-change
cargo run --offline --locked -p xtask --bin local-snapshot -- status-main worker-unique-change
cargo run --offline --locked -p xtask --bin local-snapshot -- retry-main worker-unique-change
```

The publisher makes an adjacent `push-policy` executable discoverable to its Git
child process only. The repository's configured pre-push hook still executes;
no hook, global PATH or Git configuration is disabled or changed. If the helper
is absent, the existing PATH is preserved and a hook requiring it fails closed.
Use the normal native development bootstrap first if a clean host lacks the
pinned toolchain or cached dependencies; `--offline` does not download them.

`submit` pins the clean committed HEAD, tree and resolved destination in an
immutable main-submission intent. It fetches current remote main. If necessary,
Git `merge-tree` combines source and main without touching the worker's checkout,
then `commit-tree` creates a merge with both histories as parents. Conflicts stop
without changing either branch; resolve them in the owned worker checkout and
submit a new unique name. Local source/attempt refs retain objects for recovery.

Publication is an ordinary fast-forward push. Git's receive-pack checks the
advertised old ref atomically, so one worker cannot overwrite another's commits.
After every attempt the command reads/fetches main and checks ancestry. An
accepted-but-error response is successful only when remote history contains the
candidate/source. Unreadable outcomes stop for reconciliation. Confirmed policy
rejections stop without an alternative bypass. A confirmed competing fast-forward
causes recomposition, bounded to three attempts; further contention returns a
safe retry command. Retries never select a newer HEAD silently.

Existing server policies and credential permissions are honored, not modified.
A server requiring old admission checks may reject this command until its owner
authorizes an appropriate policy change. This is an optional host policy, not a
Susi/GitHub service dependency. No force option is used.

This is cooperative fast-forward publication, not protection from administrators
who rewrite or delete main. Missing main at observation stops; deletion between
observation and push advertisement could permit ordinary Git to recreate it.
An absolute no-recreation guarantee requires a remote guard. `status-main` can
fetch missing objects for ancestry checks but never pushes or changes local refs.

## Storage guard: nothing published is lost unnoticed

The storage remote is a bucket. Susi does not depend on a hosting provider's
branch or tag protection, and `push-policy` is a
client hook: a push with `--no-verify`, or from a clone whose hooks are off, is
not checked. The guard closes that gap with Git alone:

```text
cargo run --offline --locked -p xtask --bin local-snapshot -- guard [<remote>]
```

Every clone that runs it is a witness. It reads the remote's refs
(`git ls-remote`) and keeps the last value it saw of each protected ref as a
local ref under `refs/susi-guard/<remote>/`, which also keeps those objects in
this clone. On each run:

- `refs/heads/main` and `refs/candidates/*` are append-only: the remote value
  must descend from the recorded one, or the run reports `REWOUND` with the
  number of published commits that left the history;
- `refs/tags/*` are immutable: a different value is `MOVED`;
- a recorded ref that is gone is `DELETED`, unless explicit project
  authorization directed its deletion (below).

A finding is a failed exit. It names the local ref that still holds the lost
objects and a recovery that needs no force: push the retained ref back (a
deleted ref is a plain create) or merge it into the current tip and publish a
normal fast-forward. The record of a violated ref is not advanced, so the loss
stays reported until it is repaired. A ref seen for the first time is recorded;
refs outside those namespaces (agent refs, claims) are not guarded, because
they are deleted legitimately.

### Explicitly authorized deletions

An explicitly authorized deletion is not a loss, and the guard must not direct
its restoration. The current implementation reads a committed authorization
record,
`.agents/storage/directed-deletions.txt`, one line per ref:

```text
<object> <ref> <direction date> <task>
```

`<object>` is the value the remote advertised for the ref when it was deleted
(for an annotated tag, the tag object). Blank lines and `#` lines are ignored.
The guard reads the record from the remote's own `main`, at the commit that
run observed, not from the checkout it runs in, so every clone and every
worktree of a repository sees the same record whatever it has checked out.
Susi Commercial Apps keeps its own record at the same path in its own
repository; nothing about its refs is written in this one.

- A recorded ref that is gone, listed at the object this clone last saw, is an
  accepted deletion: it is not a finding, the guard drops its retained copy
  under `refs/susi-guard/<remote>/`, and the summary line counts it once
  (`N directed deletion(s) accepted`). Later runs say nothing about it.
- A deleted ref that is not listed is `DELETED` as before, with its restore.
- A deleted ref listed at a different object than this clone saw is still
  `DELETED`, and the finding says the record names another object: something
  other than the directed deletion happened to it. A ref may have several
  lines, one per object a witness may hold.
- A listed ref that is on the remote again is `RECREATED`, in a guarded
  namespace or not (a listed `wip/*` branch included). It is not recorded, and
  the finding carries no command: do not push it again and do not delete it;
  report it on the Susi Board. Deleting it again, or withdrawing the direction
  by taking its line out of the record, requires fresh project authorization.
- A line that cannot be read accepts nothing and is reported as `UNREADABLE`
  with its line number. A repository without the file has no accepted
  deletions.
- The record counts only on a `main` that itself moved forward. If `main` is
  missing or `REWOUND` in a run, nothing is accepted in that run.

Until a replacement exists, any update to this input must describe an explicitly
authorized deletion that has been carried out, in a commit whose `Task:` names
the authorization. Never list a ref to silence a finding. This file is a
functional authorization input, not an activity log. Moving it to a local record store requires a
replacement that preserves shared authorization and recovery semantics; the
current guard does not read a local database.

`local-snapshot` calls the storage guard through the installed
`workflow_harness_v1` MCP provider. The provider must support the directed-deletion
contract; rebuilding the core command alone does not install that implementation.
An unavailable provider produces a visible audit error or advisory witness
warning, never a successful empty audit.

The guard writes nothing to the remote and nothing local except its own refs
(it creates, advances and, for an accepted directed deletion, drops them).
It works with any Git remote: a hosted service, a self-hosted server or a bare
repository on a disk. It runs from another repository's checkout too (for
example `susi-commercial-apps`) by starting the built binary there.

Limits, stated plainly: the guard detects and recovers, it does not prevent. A
clone only vouches for what it has seen, so a ref created and destroyed between
two runs of every clone is not noticed. Prevention needs a
remote under the project's administration, where Git's own
`receive.denyNonFastForwards` and `receive.denyDeletes` settings refuse the push;
those are Git, not a provider
feature, and move with a self-hosted remote.

### Every publish is a witness

No scheduler is needed. After `submit`, `retry-main`, `cut` and `retry`,
`local-snapshot` runs the guard against `origin` in the publishing clone,
whatever the command's result. Every dump to main therefore checks the remote
and refreshes that clone's record, and the busier development is, the more
often the remote is witnessed. It is advisory, because main has no gates: a
finding is printed on stderr and never changes the command's result or its
stdout. It is silent when nothing is wrong. `SUSI_NO_STORAGE_GUARD=1` skips it.

The publisher often repairs a loss by itself: a submission whose history
contains the dropped commits republishes them with its normal fast-forward. A
finding remains only when the lost commits are in no later submission, which
is exactly when someone has to act.

The harness publishes the same way. Its
[`storage_guard`](../crates/susi-harness/src/storage_guard.rs) module preserves
the public audit/report API and delegates the algorithm over MCP; `xtask`
reexports that adapter for `local-snapshot` and `local-workflow`.
`submit-main`, `integrate` and `release-promote` run the advisory witness after
live publication, from the command line and through the daemon. A finding or
unavailable-provider warning arrives beside the receipt without invalidating
or replaying an accepted publication. See the
[implementation-service boundary](architecture.md#implementation-services-and-protocol-adapters)
for registry and native environment permissions.

`cargo local-workflow ship` and `ship --retry`, the gated landing, do it as
well, whatever the ship's result. Susi Commercial Apps ships with this same
binary through its cargo alias, so its ship path is covered by the same code.

## Hooks are on after a clone's first build

Git never enables a repository's hooks on clone. The root `build.rs` therefore
sets `core.hooksPath=.githooks` the first time anything is built in a clone
whose value is unset, so `push-policy` guards pushes from fresh clones and
cloud sessions without a setup step. A deliberate value is kept, a source tree
without Git is skipped, and `SUSI_NO_HOOK_SETUP=1` opts out.
`scripts/setup-dev.sh` and `cargo xb` still configure the merge drivers.

## Snapshot now, qualify separately

From an isolated claimed worktree:

```text
cargo run --offline --locked -p xtask --bin local-snapshot -- cut 2026-11-10-rc1
cargo run --offline --locked -p xtask --bin local-snapshot -- status 2026-11-10-rc1
cargo run --offline --locked -p xtask --bin local-snapshot -- retry 2026-11-10-rc1
# On a fresh clone of the same repository:
cargo run --offline --locked -p xtask --bin local-snapshot -- adopt 2026-11-10-rc1
```

`cut` reads the full `origin/main` commit, fetches that immutable object, and
records its commit, tree and destination fingerprint in the shared repository's
Git metadata. It never uses a dirty working tree as evidence. Main may advance
while qualification proceeds. A retry always uses the original source, never the
new main. Fetch and push must resolve to one identical destination; a destination
change fails closed. Authentication is ordinary Git authentication.
Network operations use the resolved endpoint itself, so an intervening change
of the `origin` name does not redirect the request. Filesystem endpoints are
canonical absolute paths; ambiguous repeated URL rewrites and embedded URL
credentials are rejected. Git configuration, helpers and executable remain a
trusted host boundary and must not be changed concurrently by hostile processes.

The command creates the **local** branch `release/<name>` with an exact
absent-ref compare-and-swap. An existing different branch is never overwritten.
It publishes `refs/tags/susi-snapshot/<name>` with an ordinary Git push. Git's
standard tag update rule rejects replacement, including fast-forward replacement.
Concurrent publishers therefore either agree on the same snapshot or one loses.
No force option, force-with-lease, deletion or GitHub-specific protection is used.

The remote immutable snapshot is a lightweight **tag**, not a remote release
branch. Ordinary branch pushes cannot provide portable create-only semantics:
another writer can create an ancestor after the absence check, and Git can then
accept a fast-forward. Until an approved remote compare-and-swap or server policy
exists, this tool does not pretend that remote branch creation is immutable.
Other authorized clients can still force/delete tags; the tool does neither and
does not claim a security boundary against repository administrators.

## Interrupted and concurrent publication

- The intent is created exclusively and synced before any publication. An
  incomplete intent fails closed; it is never guessed, truncated or replaced.
  A competing reader of a complete intent syncs the file itself before retrying.
- Local branch creation uses `git update-ref <ref> <new> <zero>`. A competing
  branch at a different commit is preserved.
- Before a push, the remote snapshot is read. The same commit is already done;
  a different commit is a conflict; an unreadable remote means no push.
- After one push attempt, the remote is read again, even when the push command
  reports failure. A verified exact ref is success. An unreadable remote is an
  uncertain outcome, not failure or success. There is no blind automatic retry.
- `status` performs no writes. `retry` reconciles first and may push once only
  when the remote snapshot is absent. A removed local branch can be recreated;
  an existing changed branch blocks retry without changing it.

Intent files remain local Git metadata. `adopt` reads an already-published
snapshot tag into a fresh clone, binds its exact commit/tree and destination,
and creates the local release branch without a remote write. To move an
unpublished intent between machines, preserve the repository metadata. Copying
only source files does not transfer an unpublished intent.
The fingerprint is an object-format Git digest, not a credential or signature.
Power-loss durability depends on the host filesystem; a missing/partial record
requires investigation rather than selecting a different source under that name.

## What remains unchanged and unfinished

The production gate still requires exact-tree Windows x86_64 and Linux x86_64
qualification, security and license evidence, artifact identity, atomic promotion,
health checks and rollback. A snapshot is **not** qualification or a release.
The legacy `local-workflow ship` retains its old gates; the new `submit` path is
the explicitly ungated development admission route. Neither produces a release
attestation or accepts a caller-supplied "passed" qualification receipt.

Main composition and snapshot recovery are implemented here. Automatic dual-host
release qualification, qualified release-branch/tag publication, release merge
orchestration and master-brain distillation are not implemented by this slice.
The existing release tooling must still complete its full checks before any
production promotion. New sessions can invoke this CLI directly; this change
does not install a daemon, scheduled job or cross-laptop coordinator.

## Verification

The `local-snapshot` binary owns unit and real bare-Git integration tests. Run:

```text
cargo test --offline --locked -p xtask --bin local-snapshot
```

Tests cover independent-clone main submissions, history-preserving composition,
conflict refusal, exact-source retries, bounded contention, accepted-but-error
and unreadable-main responses, fresh-clone snapshot adoption, tag replacement
rejection, conflicting intents/branches, destination changes and malformed input.
Fixtures have isolated Git configuration and never use real runtime homes or keys.
Passing these focused tests is not a full workspace gate or production approval.

The built executables are also exercised together, through this repository's
own `.githooks/pre-push`, with the build directory removed from `PATH` so the
hook can reach `push-policy` only through the publisher's adjacent discovery:

```text
cargo test --offline --locked -p xtask --test git_publisher_native_hook
```

It covers a divergent-main `submit` composed and verified on the remote,
idempotent `status-main`/`retry-main`, a publisher copied away from its helper
(the hook declines, main is unchanged, and a later `retry-main` publishes the
originally pinned source, not a newer HEAD), and `cut`/`adopt` of a snapshot tag
through the hook. With discovery disabled, three of its four tests fail.

A hook that declines locally leaves `git push --porcelain` with no ref status,
which the publisher cannot tell from a connection lost mid-push; it therefore
reports an unknown outcome and points to `status-main`, never a success.

Keep measured results with the tested source and host in a local record store
outside the checkout. Re-run the checks for a new candidate; past runs do not
qualify a different tree or host. Full release qualification is separate.
