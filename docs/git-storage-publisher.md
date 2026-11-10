# Git storage publisher: first portable slice

Main is unqualified development storage. The CEO's 2026-11-10 direction removes
mandatory test gates from main admission and retains full release qualification.
This command does not run a test gate before a main submission.
No GitHub API, Actions, server extension or new Rust dependency is introduced.
The new implementation uses Rust's standard library and the installed Git tool.
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

Measured on native Windows x86_64 with pinned Rust 1.99.0: all 24 tests passed
(75.32 seconds), strict binary clippy passed (1.356 seconds), native binary build
passed (1.888 seconds), workspace format check passed (22.356 seconds), and the
actual executable's help entrypoint passed.

Measured on native Linux x86_64 (CachyOS, Rust 1.99.0, Git 2.56.0) on the
combined tree, the graft guard and push-policy discovery included
(`T-LINUXPUB-1`): all 26 `local-snapshot` tests, the 4 hook tests and the 10
`native_push_policy` tests passed; strict clippy of the publisher, helper and
hook test passed; `cargo fmt --all --check` passed. The real executable then
published `69e4e958e` to `origin/main` with `submit`. Full release qualification
remains pending. The implementation adds no dependency; the existing xtask
package still has its pre-existing dependency graph.
