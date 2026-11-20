# CEO Mandate 82: Commit, Push, Merge and Pull About Every Ten Minutes

> **History, superseded.** This record is kept for its reasoning only. The
> rules that bind agents today are in [`AGENTS.md`](../AGENTS.md), the one
> active contract; where this record differs, `AGENTS.md` wins.

Effective 2026-11-16, amended the same day by rule 6 of Mandate 83
([`docs/ceo-mandate-83.md`](ceo-mandate-83.md)): it covers committing and
pushing as well as merging and pulling, it is a cadence and not a hard limit,
and it asks for meaningful steps. Binding on every agent in the Susi AI
ecosystem — local, cloud, external (`Claude`, `Codex`, `Antigravity` /
`Carbon`, `Grok`, `DeepSeek`, `Qwen` and others) and the `Susi Harness` itself
— across both `intellibitz/susi` and `intellibitz/susi-commercial-apps`.
`.agents/identity.json` records this directive as **Mandate 82**. It adds a
rhythm to clause 3 of Mandate 74
([`docs/ceo-mandate-74.md`](ceo-mandate-74.md): work on the latest remote) and
stands beside the ten-minute integration review of Mandate 80
([`docs/ceo-mandate-80.md`](ceo-mandate-80.md)); it wins where older text
differs. `intellibitz/susi-commercial-apps` carries the paired commercial
edition in its own `docs/ceo-mandate-82.md`.

The CEO sets the vision and never inspects code. Requirements stated here are
distinct from verified runtime status; section 6 records implementation truth.

**Agent reminder (one line):** Follow CEO Mandate 82 (`docs/ceo-mandate-82.md`):
while you work, commit, push, merge and pull about every ten minutes, in each
repository you are working in — commit a meaningful step, bring in `origin/main`
(`susi-commercial workflow sync`) and publish it (`susi-commercial workflow
ship`); it is a cadence that prevents drift, not a hard limit, so never commit,
merge or push noise to meet it, and resolve a conflict, never force or skip it.

## 0. The CEO's words

> Make a mandate that every agent must merge and pull every 10 minutes.

Amended the same day (Mandate 83, rule 6):

> agents committing pushing merging and pulling every 10 minutes - not a hard
> limit but to prevent drifts must do so in meaningful ways

Recorded as visions `V-7` and `V-8` here, `V-8` and `V-9` in
`intellibitz/susi-commercial-apps`.

Many agents dump work into `main` all day. An agent that has not looked for an
hour is building on a tree that no longer exists: it repeats what has already
landed, its conflicts pile up, and it finds out only when it publishes. An
agent that keeps an hour of work to itself does the same to everyone else. Ten
minutes keeps every merge small and every agent on what the others have just
done.

## 1. The rule

1. **About every ten minutes,** every working agent commits its work, merges
   `origin/main` into the worktree it is working in, and publishes what it has
   to `main`.
2. **A cadence, not a hard limit.** Its purpose is to prevent drift. Nothing is
   refused when ten minutes pass, and a step that needs longer takes longer.
3. **Meaningfully.** Each commit, merge and publish is a real step: labelled,
   coherent, worth another agent's merge. No empty commit, no noise and no
   broken half-step pushed to satisfy a clock.
4. **Every worktree it is working in, in both repositories.** A worktree nobody
   is working in is brought up to date when work in it resumes (Mandate 74).
5. **In addition to Mandate 74's moments:** before starting, when resuming,
   before every commit and at every workflow step.
6. **A pull is a fetch and a merge.** A fast-forward or a real merge commit;
   never a rebase of shared history, never a force.

## 2. How to do it

- **Commit, merge, publish.** Commit your work on your own branch, then
  `susi-commercial workflow sync` (or `git fetch origin main && git merge
  origin/main`), then `susi-commercial workflow ship`, which fast-forwards
  `main` and posts Landed on the board.
- **See where you stand:** `susi-commercial workflow fresh` reports how far
  behind the worktree is and for how long, and how much of its own work is not
  on `main` yet and for how long.
- **A clean worktree in one step:** `susi-commercial workflow fresh --pull`.
- **Without being asked:** `susi-commercial workflow fresh --pull --watch` pulls
  now and again every ten minutes until it is stopped. Run it in the background
  of a long session.
- The tool never merges into a worktree that has uncommitted changes; it says
  so and leaves the worktree exactly as it is. Commit first.

## 3. What the cadence does not excuse

- **A conflict is resolved, never forced.** No wholesale `ours`, no discarding
  the other side, no empty or incomplete merge to keep the rhythm (Mandate 80).
  Resolve it; if the other side is another agent's live claim, post a Request
  on the Susi Board and carry on with other work (Mandates 76 and 83).
- **An unreachable remote is not a violation.** Work continues; the agent
  merges and publishes as soon as the remote answers.
- **A merge is never refused.** It is the remedy.
- **No gate is added to `main`.** Publishing is unchanged: a normal
  fast-forward, no pull request (Mandates 73 and 74).
- **A merge takes in work; it does not qualify it.** `main` stays unqualified
  development; release qualification is unchanged.
- **Not being refused is not being excused.** A note from the tool that a
  worktree has drifted is a reason to merge and publish now.

## 4. How it fits the other mandates

- **Mandate 74, clause 3** has every agent on the latest remote before it
  starts, when it resumes and before each commit, and refuses a commit more
  than 20 commits behind. Both stay; that refusal is the only one.
- **Mandate 80** has every agent review its ready work for integration every
  ten minutes. This is the same rhythm, in both directions: take work in, hand
  work out.
- **Mandate 76:** a merge of `intellibitz/susi-commercial-apps` also brings in
  the board items that cloud agents post through `main`.
- **Mandate 83, rule 3:** publishing work on a task renews its claim, so the
  cadence is also how an agent keeps the claims it is working on.

## 5. Every agent, every session

Start the session with `susi-commercial workflow check` (it pulls a clean
worktree). While working, keep the rhythm of section 2. When the tool notes
that the worktree has drifted, merge and publish then.

## 6. Implementation truth (2026-11-16)

- **Built in the workflow tool (Commercial `T-CLAUDEMANDATE82-1`, amended by
  `T-CLAUDEMANDATE83-1`):** `susi-commercial` keeps, in each worktree's own Git
  directory, the moment the worktree was first seen behind `origin/main` and
  the remote commit it lacked then. The record is dropped when the worktree is
  current and started afresh when it has merged that commit and the remote has
  moved again, whether the merge was made with the tool or with plain Git. The
  tool also counts the worktree's own commits that are not on `origin/main`
  yet and how long the oldest has waited.
- **Measured and said, never refused:** `susi-commercial workflow fresh`, and
  the commit guard that this repository's commit hooks run when the tool is
  installed, report both. Past ten minutes the note is plainer. Nothing is
  refused on time.
- **Withdrawn:** on the day this mandate was issued the tool refused a commit
  once a worktree had been behind for more than ten minutes (after one saving
  commit). The CEO made the ten minutes a cadence the same day and the refusal
  was removed.
- **Still refused:** a commit more than 20 commits behind `origin/main`
  (Mandate 74). A merge is never refused.
- **Automatic:** `susi-commercial workflow fresh --pull --watch` (section 2).
- **Not enforced:** nothing wakes an agent at ten minutes. An agent that
  neither commits nor runs the tool is not timed, and the clock starts when the
  tool first sees the worktree behind, not when the remote moved. Whether a
  commit or a publish is a meaningful step is not measured. A worktree with
  uncommitted changes is never merged by the tool. A `susi-commercial` built
  before these changes does not measure time, and one built between them still
  refuses, until the host rebuilds it from `main`. This mandate's presence is
  guarded by `tests/architecture_tests.rs`.

This document starts no timer, merges nothing and certifies no agent as
compliant.
