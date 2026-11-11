# Agent refs: `refs/agents/<agent>`

CEO decision, 2026-11-11: agent inbox branches leave GitHub's branch namespace,
so no agent push ever creates a pull-request surface. Agents publish their work
to the non-branch ref `refs/agents/<agent>`. `refs/heads/main` is the only
branch on the storage remote.

## Why

GitHub offers "Compare & pull request" for every recently pushed
`refs/heads/*` branch, and lets anyone open a pull request against any branch.
Each push of `refs/heads/local-agent/<agent>` therefore put a merge button next
to Susi's own integration path, the same surface the earlier `release/*` and
`distill/*` branches had. A ref outside `refs/heads/` is plain Git storage:
GitHub stores and serves it to `git fetch`, shows no banner for it and cannot
open a pull request from or into it.

## Ref layout

| Where | Ref | What |
|---|---|---|
| Agent worktree (local) | `refs/heads/local-agent/<agent>` | The branch the agent works and commits on. The name is unchanged: worktrees, claims and `cargo local-workflow start` still use it. |
| Storage remote | `refs/agents/<agent>` | The agent's published work. Created, fast-forwarded, or deleted at its tip. |
| Storage remote | `refs/heads/main` | Unqualified development main, the only remote branch. |
| Integrating host | `refs/remotes/<remote>-agents/<agent>` | Local mirror of the remote's `refs/agents/*` (for `origin`: `refs/remotes/origin-agents/<agent>`). |

`<agent>` is one ref component (no `/`), the same rule `local-agent/<agent>`
used. Agent refs, like candidates and claims, never appear in `git branch -r`
on a plain clone, and a plain `git fetch` does not download them.

## Push rules (`push-policy`, core `xtask/src/push_policy.rs`)

The pre-push hook (`.githooks/pre-push`) runs `push-policy`, which admits:

- `refs/agents/<agent>`: creation; a normal fast-forward update (the old tip
  must be an ancestor of the new one, so `--force` rewinds are refused); a
  normal deletion naming the tip the remote advertised.
- `refs/heads/local-agent/<agent>`: deletion only, to retire the old branches.
  Creating or updating one is refused with a message naming
  `refs/agents/<agent>`.
- Every other rule is unchanged: `refs/heads/main` by fast-forward only and
  never deleted; other branch heads may only be deleted; candidate refs
  fast-forward; tags are immutable and `vX.Y.Z` needs its qualification record;
  claim and close-receipt refs are blobs.

The commercial repository's `.githooks/pre-push` enforces the same rules.

## Publishing (agent)

```sh
git push origin local-agent/<agent>:refs/agents/<agent>
```

Repeat it after each batch of commits; it is a fast-forward. Fast-main agents
publish to `main` directly as before; the agent ref is for work that a gated
host or the governor should integrate, or for an off-host backup.
`susi_harness::workflow::IntegrationPolicy` gives every worker the same rule:
`publish_branch` is the local branch, `publish_ref` the remote ref, and
`check_push_target` refuses `local-agent/*` targets with `RetiredAgentBranch`.

## Inspecting

```sh
git ls-remote origin 'refs/agents/*'          # every published agent ref
git ls-remote origin refs/agents/<agent>      # one agent
```

## Fetching and adopting (integrating host)

Fetch the agent refs into the mirror:

```sh
git fetch --prune origin '+refs/agents/*:refs/remotes/origin-agents/*'
```

`--prune` with this explicit refspec removes mirror entries whose agent ref was
deleted on the remote and touches nothing else. To fetch them on every
`git fetch`, add the refspec once:
`git config --add remote.origin.fetch '+refs/agents/*:refs/remotes/origin-agents/*'`.

Then integrate in Susi, never on the host:

- `susi-harness integrate` (`git_governor::sweep_pending_branches`) runs that
  fetch itself, sweeps every agent ref together with the remote branches, and
  reports each under its worktree name `local-agent/<agent>` (`git_ref` names
  the mirror ref). `--branch 'local-agent/<agent>'` filters as before, `RETIRE`
  verdicts recorded for `local-agent/<agent>` (or `refs/agents/<agent>`) still
  apply, and a legacy branch at the same tip as the agent ref is one candidate.
- `susi-harness submit-main --rev refs/remotes/origin-agents/<agent>` submits
  one fetched agent ref.
- A gated host merges `refs/remotes/origin-agents/<agent>` into a claimed
  worktree and ships it with `cargo local-workflow ship <claim>`. The core
  `local-workflow` tool reads no remote agent branch itself; it gates the
  claimed worktree it is given.

## Transition

1. Both repositories' push policies admit `refs/agents/*` and refuse new
   `refs/heads/local-agent/*` pushes.
2. Each existing remote `refs/heads/local-agent/<name>` is preserved by pushing
   its tip, create-only, to `refs/agents/<name>`; `git ls-remote` confirms the
   two are equal; only then is the branch deleted with a normal push deletion.
   A branch whose tip moved in the last 30 minutes is preserved but not deleted
   (its agent may be mid-push).
3. Until the last legacy branch is gone, the governor still reads
   `refs/remotes/<remote>/local-agent/*`, read-only; nothing writes them.
