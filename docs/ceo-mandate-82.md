# CEO Mandate 82: Every Agent Pulls and Merges `origin/main` Every Ten Minutes

Effective 2026-11-16. Binding on every agent in the Susi AI ecosystem — local,
cloud, external (`Claude`, `Codex`, `Antigravity` / `Carbon`, `Grok`, `DeepSeek`,
`Qwen` and others) and the `Susi Harness` itself — across both `intellibitz/susi`
and `intellibitz/susi-commercial-apps`. `.agents/identity.json` records this
directive as **Mandate 82**. It tightens clause 3 of Mandate 74
([`docs/ceo-mandate-74.md`](ceo-mandate-74.md): work on the latest remote) and
stands beside the ten-minute integration review of Mandate 80
([`docs/ceo-mandate-80.md`](ceo-mandate-80.md)); it wins where older text
differs. `intellibitz/susi-commercial-apps` carries the paired commercial
edition in its own `docs/ceo-mandate-82.md`.

The CEO sets the vision and never inspects code. Requirements stated here are
distinct from verified runtime status; section 6 records implementation truth.

**Agent reminder (one line):** Follow CEO Mandate 82 (`docs/ceo-mandate-82.md`):
while you work, pull and merge `origin/main` into your worktree at least every
ten minutes, in each repository you are working in — commit your work, then
`git fetch origin main && git merge origin/main` (or `susi-commercial workflow
sync`); resolve a conflict, never force or skip it; a worktree left behind for
more than ten minutes gets one commit to save its work and is refused every
commit after it until it has merged.

## 0. The CEO's words

> Make a mandate that every agent must merge and pull every 10 minutes.

Recorded as vision `V-7` here and `V-8` in `intellibitz/susi-commercial-apps`.

Many agents dump work into `main` all day. An agent that has not looked for an
hour is building on a tree that no longer exists: it repeats what has already
landed, its conflicts pile up, and it finds out only when it publishes. Ten
minutes keeps every merge small and every agent on what the others have just
done.

## 1. The rule

1. **Every ten minutes, at the latest,** every working agent fetches
   `origin/main` and merges it into the worktree it is working in.
2. **Every worktree it is working in, in both repositories.** A worktree nobody
   is working in is brought up to date when work in it resumes (Mandate 74).
3. **In addition to Mandate 74's moments:** before starting, when resuming,
   before every commit and at every workflow step. Those stay; this adds the
   clock between them.
4. **A pull is a fetch and a merge.** A fast-forward or a real merge commit;
   never a rebase of shared history, never a force.

## 2. How to do it

- **Commit, then merge.** Commit your work on your own branch (labelled work in
  progress is welcome: Mandate 74 dumps labelled work), then
  `git fetch origin main && git merge origin/main`, or
  `susi-commercial workflow sync`.
- **A clean worktree in one step:** `susi-commercial workflow fresh --pull`.
- **Without being asked:** `susi-commercial workflow fresh --pull --watch` pulls
  now and again every ten minutes until it is stopped. Run it in the background
  of a long session.
- The tool never merges into a worktree that has uncommitted changes; it says
  so and leaves the worktree exactly as it is. Commit first. That is why a
  worktree that is already late is still allowed one commit: the commit saves
  the work, and the merge follows it.

## 3. What the clock does not excuse

- **A conflict is resolved, never forced.** No wholesale `ours`, no discarding
  the other side, no empty or incomplete merge to satisfy the clock (Mandate
  80). Resolve it; if the other side is another agent's open claim, ask the
  owner on the Susi Board (Mandate 76).
- **An unreachable remote is not a violation.** Work continues; the agent
  merges as soon as the remote answers.
- **A merge is never refused.** It is the remedy.
- **No gate is added to `main`.** Publishing is unchanged: a normal
  fast-forward, no pull request (Mandates 73 and 74).
- **A merge takes in work; it does not qualify it.** `main` stays unqualified
  development; release qualification is unchanged.

## 4. How it fits the other mandates

- **Mandate 74, clause 3** already has every agent on the latest remote before
  it starts, when it resumes and before each commit, and refuses a commit more
  than 20 commits behind. Both stay. This mandate adds time: ten minutes.
- **Mandate 80** has every agent review its ready work for integration every
  ten minutes. That is handing work out; this is taking work in. One clock, two
  directions.
- **Mandate 76:** a merge of `intellibitz/susi-commercial-apps` also brings in
  the board items that cloud agents post through `main`, so the ten-minute pull
  is also how those reach every agent.

## 5. Every agent, every session

Start the session with `susi-commercial workflow check` (it pulls a clean
worktree). While working, merge at least every ten minutes as in section 2.
When the tool notes that the worktree is behind, merge then; do not wait for
the refusal. If the ten minutes have passed with work uncommitted, commit it
(that one commit is let through), then merge.

## 6. Implementation truth (2026-11-16)

- **Built (Commercial `T-CLAUDEMANDATE82-1`):** the workflow tool
  `susi-commercial` keeps, in each worktree's own Git directory, the moment the
  worktree was first seen behind `origin/main` and the remote commit it lacked
  then. The record is dropped when the worktree is current and started afresh
  when it has merged that commit and the remote has moved again, whether the
  merge was made with the tool or with plain Git.
- **Enforced in code today:** the commit guard
  (`susi-commercial workflow guard commit`, which this repository's commit
  hooks run when that tool is installed) acts when the remote answers and the
  worktree has been behind for more than ten minutes. It lets one commit
  through, so the work is saved and the worktree is clean for the merge, and
  says so. Every commit after that one is refused until the worktree has
  merged, and the refusal says how. A commit that was let through but failed
  for another reason may be tried again. Every refusal follows a notice given
  at least ten minutes earlier, because the clock starts at the first notice.
  There is no override and no setting for the ten minutes. A merge, including
  a conflicted merge concluded with a commit, is never refused.
- **Measured:** `susi-commercial workflow fresh` and `workflow check` report how
  many commits a worktree is behind and for how many minutes.
- **Automatic:** `susi-commercial workflow fresh --pull --watch` (section 2).
- **Not enforced:** nothing wakes an agent at ten minutes. An agent that
  neither commits nor runs the tool is not timed, and the clock starts when the
  tool first sees the worktree behind, not when the remote moved. A worktree
  with uncommitted changes is never merged by the tool. A `susi-commercial`
  built before this change enforces only the 20-commit limit, so each host
  enforces this once it has rebuilt the tool from `main`. A commit made with
  hooks switched off is not checked. This mandate's presence is guarded by
  `tests/architecture_tests.rs`.

This document starts no timer, merges nothing and certifies no agent as
compliant.
