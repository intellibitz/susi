# CEO Mandate 83: The Ten Agent Operating Rules

Effective 2026-11-16. Binding on every agent in the Susi AI ecosystem — local,
cloud, external (`Claude`, `Codex`, `Antigravity` / `Carbon`, `Grok`, `DeepSeek`,
`Qwen` and others) and the `Susi Harness` itself — across both `intellibitz/susi`
and `intellibitz/susi-commercial-apps`. `.agents/identity.json` records this
directive as **Mandate 83**. It puts in one place how an agent works. Where a
rule is already carried by another mandate it points there and does not repeat
it; where it is new, or changes an older mandate
([`docs/ceo-mandate-76.md`](ceo-mandate-76.md),
[`docs/ceo-mandate-82.md`](ceo-mandate-82.md)), it wins.
`intellibitz/susi-commercial-apps` carries the paired commercial edition in its
own `docs/ceo-mandate-83.md`.

The CEO sets the vision and never inspects code. Requirements stated here are
distinct from verified runtime status; section 3 records implementation truth.

**Agent reminder (one line):** Follow CEO Mandate 83 (`docs/ceo-mandate-83.md`):
never wait on another agent; hold no claim you are not working on; keep each
claim's progress up to date or lose it to another agent; always collaborate
through the Susi Board; always work isolated in your own worktrees, which you
may reuse; commit, push, merge and pull about every ten minutes, meaningfully,
as a cadence against drift and not a hard limit; work in both repositories;
reach secrets only through the vault and never show a value; run fully
autonomous with full permissions; and bring a solution with every problem.

## 0. The CEO's words

> create,edit or add to mandate - 1. agents cannot wait on other agents 2. agents
> cannot hold a claim and not be working on it 3. agents must keep updating
> progress on a claim, or the claim can be claimed by other agents 4. agents
> always collaborating with each other 5. agents always isoloated, work on their
> own isolated worktrees and can reuse their own worktrees 6. agents committing
> pushing merging and pulling every 10 minutes - not a hard limit but to prevent
> drifts must do so in meaningful ways 7. agents must be able to work on both
> susi core and susi commercial apps 8. agents must access secrets in a safe and
> secure ways 9. agents always full autonomous and full permissions mode on 10.
> agents being proactive in providing solutions to problems.

Recorded as vision `V-8` here and `V-9` in `intellibitz/susi-commercial-apps`.

## 1. The ten rules

1. **Never wait on another agent.** When you need something from another
   agent, post a Request on the Susi Board and carry on with other work. When
   another agent holds a live claim on what you wanted, take other work. When
   its claim has lapsed, take the task (rule 3). Waiting is not a state an
   agent is in.
2. **Hold no claim you are not working on.** Claim when you start. Release when
   you stop (`susi-commercial tasks release <id>`, which posts the Handoff).
   Never park a claim for later.
3. **Keep a claim's progress up to date, or lose it.** A claim lives one hour
   at a time. Publishing work that names the task renews it;
   `susi-commercial tasks renew <id>` renews it when there is nothing to
   publish yet. A claim nobody keeps alive lapses, and any agent may then take
   the task with `susi-commercial tasks claim <id>`, without asking. The
   earlier holder's work is on `main` already, because of rule 6.
4. **Always collaborate.** One agent or many: read the board before you work,
   post your claims and results, answer what is addressed to you. Detail:
   Mandate 76.
5. **Always isolated; your own worktrees are yours to reuse.** Work only in
   your own worktrees under your own agent root, never in another agent's or
   the CEO's. You may return to a worktree of your own for further work
   instead of making a new one each time; bring it up to date first
   (`susi-commercial workflow check`). Detail: Mandates 49, 65, 74 and 77.
6. **Commit, push, merge and pull about every ten minutes, meaningfully.** Ten
   minutes is a cadence that prevents drift, not a hard limit: nothing is
   refused when it passes. Each commit, publish and merge is a meaningful
   step: no empty commit, no noise and no broken half-step pushed to satisfy a
   clock. Detail: Mandate 82, amended by this rule.
7. **Work in both repositories.** Every agent can work in `intellibitz/susi`
   and in `intellibitz/susi-commercial-apps`, keeps its own clone of each under
   its root, and uses the same workflow tool and the same board from either.
   What never crosses is content: commercial code and board items never enter
   the public core.
8. **Reach secrets safely.** Only through the vault and the process
   environment, handed to the consumer and never shown: not in chat, a log, a
   commit, a file or the board. Report at most that a value is set and its
   length. Never open the CEO's Personal vault. Detail: Susi Commercial Apps
   `AGENTS.md` sections 5 and 6; core `susi keys vault`.
9. **Fully autonomous, full permissions on.** Agents run in their tool's
   full-permission mode and decide, commit, push, merge and publish without
   asking the CEO for routine approval. Full permission is not permission to
   do harm. Every boundary the other mandates set stays: no force push and no
   rewritten history, no deletion or other destructive action without CEO
   direction, no spending or publication outside the task, secrets as in rule
   8, nothing changed in another agent's root, and nested work never holds
   more permission than the agent that delegated it. Detail: Mandates 62 and
   73.
10. **Be proactive: bring the solution.** An agent that finds a problem fixes
    it when that is inside its task and authority. Otherwise it records a task
    and posts it on the board with the fix it proposes. It never reports a
    problem alone and stops.

## 2. What this changes in older mandates

- **Mandate 82** (ten minutes): now covers committing and pushing as well as
  merging and pulling, is a cadence and not a hard limit, and asks for
  meaningful steps. The commit refusal it introduced is withdrawn.
- **Mandate 76** (the board): asking another agent never means waiting for
  it, and a claim past its lease is open to any agent without asking its old
  holder.
- **Claim leases:** one hour whatever the task's size, renewed by progress.
  It was four hours, and twelve for a large task.

Everything else in those mandates stands.

## 3. Implementation truth (2026-11-16)

- **Built in the workflow tool (Commercial `T-CLAUDEMANDATE83-1`), which this
  repository's hooks run when it is installed:**
  - *Rule 3.* A claim's lease is one hour. `susi-commercial workflow ship`
    renews the leases the publisher holds on the tasks its publish carried, and
    `tasks renew` renews by hand. `tasks claim` is atomic and refuses only
    while another agent's lease is live. The board lists claims past their
    lease as open to any agent.
  - *Rule 3, for a host that may not write claim refs (Commercial
    `T-CLAUDECLOUDCLAIMS-1`).* A cloud session's storage credentials refuse
    claim refs. It now claims, renews and releases through `main`: the claim
    is one file, `.agents/claims/<task>.json`, committed to the `main` of the
    repository the task belongs to, on the tip that was read, so among such
    hosts one wins. It holds what a claim ref on the same remote already
    shows: a task, an agent, a branch, paths and a lease. Every tool reads
    both stores, and the lease, the one-task-per-agent rule and scope
    reservations decide on both. Across the two stores one holder is kept by
    a second look: each side writes to its own store, reads the other again,
    and withdraws when a rival arrived there since its first look. Run from
    a cloud session against this repository on 2026-11-16.
  - *Rule 2, closing from such a host (Commercial `T-CLAUDECLOUDCLOSE-1`).*
    `tasks close` works there too. The close receipt ("accepted, not yet on
    `main`") is one more file on `main`, `.agents/closed/<task>.json`, read by
    every tool beside the receipt refs: it keeps the task's paths reserved and
    its agent in debt until the close is published, as a receipt ref does.
    `workflow ship` then settles the publisher's claims on every host: work
    landed on an open task renews the lease, and a task whose completed record
    the publish put on `main` has its claim released and its receipt settled.
    The task that wrote this paragraph was closed that way.
  - *Rule 2, `workflow finish` from such a host (Commercial
    `T-CLAUDEFINISHMAIN-1`).* `finish` closes with the receipt held back,
    then releases the claim and publishes the receipt together, so the debt
    is on the remote the moment ownership is gone. For a claim kept on `main`
    that step is now one commit on `main` that removes the claim file and adds
    the receipt file; before, the release refused there.
  - *Rule 6.* The ten minutes refuse nothing. Past them the commit guard and
    `workflow fresh` say plainly that a worktree is behind `origin/main`, or
    that its own commits wait unpublished, and for how long. The 20-commit
    limit of Mandate 74 still refuses a commit.
  - *Rule 7.* Board commands run from a checkout of this repository use the
    agent's `susi-commercial-apps` clone, and a board item cannot be sent to
    this repository's `main`.
- **Built here (`T-CLAUDEMANDATE83-2`, `crates/susi-core/src/native_board.rs`):**
  this repository's own board check finds that clone too, under the agent's
  root, when nothing else names a board. `susi-commercial workflow sync`
  therefore runs in a core checkout with nothing set by hand; before, it
  failed there unless the board's address was given in a variable.
- **Already in place:** rule 5 through agent roots and isolated worktrees
  (Mandates 49, 65 and 77). Returning to a worktree of your own needs no
  command: enter it and run `susi-commercial workflow check`, which pulls it up
  to date. Rule 9 in how the Susi Harness launches engines: its default engine
  commands use each tool's full-permission flag
  (`crates/susi-harness/src/adaptive.rs`), and a governed profile replaces that
  flag where an operator selects it. Rule 8 through `susi keys vault`.
- **Not enforced:** rules 1, 2, 4 and 10 are conduct. The tool cannot see an
  agent waiting, idle or silent about a fix; rule 2 is enforced only through
  the lease, so an idle claim lapses within the hour. Whether a commit or a
  publish is a meaningful step is not measured. A claim kept on `main` adds one
  commit to `main` per claim, renewal, close and release. The reason of an
  abandoned task is recorded on a ref a cloud session cannot write; from such
  a session the board item carries it. The one-hour
  lease is the implementing agent's choice of number; the CEO named none. A
  tool built before this change keeps the longer leases, and the ten-minute
  refusal if it was built in between, until it is rebuilt from `main`; one
  built before claims and receipts were kept on `main` does not see them. This
  repository does not set the permission mode of an agent a person starts by
  hand. This mandate's presence is guarded by `tests/architecture_tests.rs`.

This document starts no agent, grants no access and certifies no agent as
compliant.
