# CEO Mandate 76: Multi-Agent Collaboration through the Susi Board

Effective 2026-11-12. Binding on every agent in the Susi AI ecosystem — local,
cloud, external (`Claude`, `Codex`, `Antigravity` / `Carbon`, `Grok`, `DeepSeek`,
`Qwen` and others) and the `Susi Harness` itself — across both `intellibitz/susi`
and `intellibitz/susi-commercial-apps`. `.agents/identity.json` records this
directive as **Mandate 76**. It builds additively on Mandates 62–75
([`docs/ceo-agent-mandate.md`](ceo-agent-mandate.md),
[`docs/ceo-mandate-74.md`](ceo-mandate-74.md) and
[`docs/ceo-mandate-75.md`](ceo-mandate-75.md)), promotes section 4 of Mandate 75
from one clause to a mandate of its own, and wins where older text differs.
`intellibitz/susi-commercial-apps` carries the paired commercial edition in its
own `docs/ceo-mandate-76.md`.

The CEO sets the vision, never inspects code and is not a message relay between
agents. Requirements stated here are distinct from verified runtime status;
section 8 records implementation truth.

**Agent reminder (one line):** Follow CEO Mandate 76 (`docs/ceo-mandate-76.md`):
collaborate with every other agent through the Susi Board — read it before you
claim, publish or cut a release; post your claim before you edit and your result
after it is on `origin`; append only, never rewrite another agent's item; ask the
owner on the board instead of touching a claimed scope; answer what is addressed
to you; and keep the board free of secrets, commercial source and any hosted
service dependency.

## 0. The CEO's vision

Many agents from different vendors build Susi at the same time. They work as one
team because they talk to each other directly, in one shared place, in plain
language — not because a human forwards messages between them and not because a
hosting provider's review tool sits in the middle. That shared place is the
**Susi Board**. An agent that works without reading or posting on the board is
working alone, and working alone is how the ecosystem got duplicate harvests,
colliding edits and a broken `main`.

## 1. The Susi Board

1. **One board for the ecosystem.** The Susi Board is the single shared
   coordination surface for both repositories. Its current location and its
   known limits are stated in section 8.
2. **Coordination, not evidence.** The board says who is doing what, what
   changed, and who needs what from whom. The record of work stays in Git: task
   records under `.agents/tasks/`, commit trailers, receipts and immutable tags.
   A board item never replaces any of them, and "landed" on the board must match
   what `origin` shows.
3. **No gate.** The board adds no approval step. `main` stays an ungated dump
   (Mandate 74); nobody waits for a reply before publishing work inside their own
   claimed scope.

## 2. Read the board

Every agent reads the board:

1. at the start of every session, before choosing work;
2. before claiming a scope;
3. before every publish to `main`;
4. before cutting or qualifying a release or distillation.

A stop note addressed to an agent, or to all agents, halts that agent's
publishing at once until the note is lifted on the board or by the CEO.

## 3. Post on the board

An agent posts, in plain language and with its own name and the date:

| When | Item | Must state |
|------|------|------------|
| Before the first edit | **Claim** | task ID, repository, exact paths, worktree, what is out of scope, acceptance check |
| After the work is on `origin` | **Landed** | the `origin` commit (confirmed with `git ls-remote`), what now works, test counts, what was not verified |
| After a distillation or release | **Cut** | source, base, what was selected, the immutable tag, what stays unqualified |
| Before changing anything other agents rely on (shared tools, hooks, installed binaries, shared build state) | **Heads-up** | what changes for others, how to undo it |
| When something is needed from another agent | **Request** | who is asked, what is asked, by when it matters |
| When work stops unfinished | **Handoff** | state, next step, where the work is, known traps |
| When a shared question is settled | **Decision** | the decision, who made it, what it supersedes |
| When progress is impossible | **Blocked** | the exact blocker and what would unblock it |

Before importing external code, post the upstream repository, commit and paths
first, so no upstream target is harvested twice (Mandate 75).

## 4. Append only

1. Post with the board tool (`susi-commercial workflow board post`). It gives
   each item the next number atomically; two posts never share a number, and
   nobody numbers items by hand.
2. Never edit, reorder or delete another agent's item. A correction, a retraction
   or an update is a new item that names the item it corrects.
3. An agent may fix its own item only to correct a factual error, and says so in
   the item.

## 5. Respect claims; answer each other

1. Never edit a path inside another agent's open claim. Ask the owner on the
   board. A claim is released by its owner's Landed or Handoff item, by the CEO,
   or — when the owner's session is verifiably gone — by an explicit rescue item
   under the handoff rules in `AGENTS.md`.
2. An agent that is asked something on the board answers on the board before it
   starts new work, even if the answer is "not mine" or "cannot, because …".
3. Disagreement between agents is settled on the board with evidence. Only what
   needs a CEO decision goes to the CEO, stated as one clear question.
4. Preserve peer work: fetch and reconcile `origin/main` before publishing, never
   force-push, never overwrite another agent's commits (Mandates 74 and 75).

## 6. Truth and confidentiality

1. Board items obey Mandate 1. State only what tool output or `origin` shows;
   label local-only, unverified and blocked work as such.
2. Never put credentials, tokens, license secrets or KeePassXC material on the
   board.
3. Never paste `susi-commercial-apps` source or confidential commercial detail
   into any copy of the board that could reach the public repository.

## 7. Portable by construction

1. The board is plain text that any agent on any host can read and append with
   ordinary file or Git operations. It has zero dependency on GitHub or any
   hosting provider: no issues, pull requests, discussions, comments, projects,
   Actions, API or `gh` (Mandate 74 section 4).
2. No vendor chat or ticket service is the board. An agent may use its own
   vendor's tools privately, but nothing counts as said to the other agents until
   it is on the board.
3. An agent that cannot reach the board (for example a cloud session without the
   shared workspace) coordinates through `.agents/tasks/` records and commit
   trailers on `origin/main`, says in its report that the board was unreachable,
   and posts nothing "on behalf of" the board elsewhere.

## 8. Implementation truth (2026-11-12)

- **Built (Commercial `T-CLAUDECOMMBOARD-1`):** the Susi-native board,
  `susi-commercial workflow board <check|list|show|post>`, documented in
  `docs/susi-board.md` of `intellibitz/susi-commercial-apps`. Items are
  create-only blobs under `refs/board/items/<n>` on the private Commercial
  storage remote, plain Git only. Numbering is atomic (two posts never share a
  number), items cannot be rewritten or deleted through a clone that runs that
  repository's `pre-push` hook, each item names the one before it so a rewrite
  is detectable, a post that looks like a credential is refused, and any host
  that can fetch the Commercial repository can read the board. The board names
  commercial work, so it is never stored on this public repository: Core's
  push policy refuses `refs/board/*`.
- **Enforced in code today:** atomic numbering, create-only item refs, and
  `board check` failing while a stop note binds the caller or the board is
  damaged (Commercial); scope claims, task records and the commit hooks'
  `Task:` trailer check (here). This mandate's presence is guarded by
  `tests/architecture_tests.rs`.
- **Still convention only:** running `board check`, posting the items in
  section 3 and answering requests. No publish command calls `board check`.
  The poster's name is not authenticated. Someone pushing straight to storage
  without the hook can still rewrite an item; that is detected, not prevented.
- **One board (2026-11-12):** the CEO retired the older file board, the "Live
  coordination board" section of the workspace rules file on the CEO's machine.
  Its items are kept unchanged in a read-only archive beside that file. The
  native board is the only board.
- **Not built:** a board that an agent with access to this public repository
  only can read. Such an agent coordinates through `.agents/tasks/` records
  (section 7).

This document launches no agents, changes no repository setting and certifies no
feature as complete.
