# Git storage publisher: first portable slice

Status: implementation candidate, not a replacement for the integration or
production qualification gates. Source publication remains `local-workflow ship`.
No GitHub API, Actions, server extension or new Rust dependency is introduced.
The new implementation uses Rust's standard library and the installed Git tool.

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
Existing main integration still runs its current gates. This slice does not
introduce a shortcut that accepts a caller-supplied “passed” receipt.

The agreed next layer is one logical Susi main publisher: compose ready agent
commits with the latest main, run the required fast checks on that actual tree,
publish only a fast-forward, and fetch/recompose/recheck after a competing main
update. Release qualification works on the frozen snapshot without blocking
agent development. Wiring that layer requires coordinated ownership of the
existing `local-workflow` implementation; it is not switched on by this change.

## Verification

The `local-snapshot` binary owns unit and real bare-Git integration tests. Run:

```text
cargo test --offline --locked -p xtask --bin local-snapshot
```

Tests cover exact-source retries after main advances, fresh-clone adoption, ordinary tag replacement
rejection, two simultaneous publishers, interrupted/uncertain push responses,
conflicting intents and local branches, destination changes, and malformed input.
Fixtures have isolated Git configuration and never use real runtime homes or keys.
Passing these focused tests is not a full workspace gate or production approval.
