# CEO Mandate 77: One Isolated Working Directory per Agent (Agent Roots)

Effective 2026-11-12. Binding on every agent in the Susi AI ecosystem — local,
cloud, external (`Claude`, `Codex`, `Antigravity` / `Carbon`, `Grok`, `DeepSeek`,
`Qwen` and others) and the `Susi Harness` itself — across both `intellibitz/susi`
and `intellibitz/susi-commercial-apps`. `.agents/identity.json` records this
directive as **Mandate 77**. It builds additively on Mandates 49, 65 and 73–76
([`docs/ceo-agent-mandate.md`](ceo-agent-mandate.md),
[`docs/ceo-mandate-74.md`](ceo-mandate-74.md),
[`docs/ceo-mandate-76.md`](ceo-mandate-76.md)) and wins where older text
differs. `intellibitz/susi-commercial-apps` carries the paired commercial
edition in its own `docs/ceo-mandate-77.md`.

The CEO sets the vision and never inspects code. Requirements stated here are
distinct from verified runtime status; section 7 records implementation truth.

**Agent reminder (one line):** Follow CEO Mandate 77 (`docs/ceo-mandate-77.md`):
work only under your own agent root — one directory per agent per machine, named
for the agent and marked with `.susi-agent-root` — keep every clone, worktree,
build target and scratch file you create inside it, never create, change or
delete anything under another agent's root or the CEO's, and ask the owner on
the Susi Board when you need something from theirs.

## 0. The CEO's vision

Many agents work on one machine at the same time. Each must have a place that is
unmistakably its own, so that no two agents ever pick the same directory, build
into each other's files, or clean up each other's work. That place is the
agent's **root**: a root context per agent — `claude` for the Claude agent,
`codex` for Codex, and so on. Isolated worktrees (Mandates 49 and 65) keep two
*tasks* apart; agent roots keep two *agents* apart.

## 1. One root per agent per machine

1. Every agent has exactly one root directory on each machine it works on.
2. The root is named for the agent. A new root is `$SUSI_AGENTS_HOME/<agent>`,
   or `~/susi-agents/<agent>` when that variable is unset. A root that already
   exists elsewhere stays where it is and is registered in place; nothing is
   moved to satisfy this mandate.
3. A root is marked by a file `.susi-agent-root` at its top that names the
   owning agent. The marker is the registration: there is no central list to
   keep in step.
4. Roots never nest and never overlap. A directory has one owner.
5. Several sessions of the same agent share that agent's root and keep apart
   inside it by worktree and task name (section 3).

## 2. Everything an agent creates lives under its root

1. Its own clones of `intellibitz/susi` and `intellibitz/susi-commercial-apps`.
2. Every worktree, created beside those clones.
3. Every build target, cache it owns, private `SUSI_HOME`, log and scratch file.
4. Files that must live in a shared temporary directory carry the agent's name
   and a unique suffix, and are removed by the agent that made them.

## 3. Names inside a root never collide

1. A worktree directory and its branch carry the agent's name and the task or a
   unique stamp (`susi-commercial workflow start` generates
   `<agent>-<date>-<time>-<pid>` when no name is given).
2. An existing directory is reused only by the session that owns it. A session
   that finds a directory it did not create picks another name; it never
   adopts, resets or deletes it.

## 4. Never write in another agent's root

1. An agent never creates, edits, moves or deletes anything under another
   agent's root, and never under the CEO's own working directory.
2. Reading another root is allowed. Fetching from another agent's clone with
   plain Git is allowed. Building there is not: a build writes.
3. To get something changed in another root, ask its owner on the Susi Board
   (Mandate 76).
4. A rescue of another agent's unfinished work is done by fetching it into
   one's own root. Working inside the other root itself needs the owner's or
   the CEO's word, is announced on the board first, and uses the explicit
   override in section 7.

## 5. What stays shared

1. The storage remotes and the Susi Board.
2. Tools installed for all agents (for example `~/.cargo/bin`) and the warm
   build snapshots agents copy from. Changing a shared thing needs a Heads-up
   on the board first (Mandate 76 section 3).
3. The workspace rules file. It is read by all and edited only as its own rules
   allow.

## 6. Every agent, every session

1. Find your root: `susi-commercial workflow root show`. If you have none,
   register one: `susi-commercial workflow root init --agent <name>`.
2. Work only in clones and worktrees under it.
3. If the tool says a directory belongs to another agent, stop and move to your
   own root. Do not override it to save time.

## 7. Implementation truth (2026-11-12)

- **Built (Commercial `T-CLAUDECOMMAGENTROOT-1`):**
  `susi-commercial workflow root <show|init>`, documented in
  `docs/agent-roots.md` of `intellibitz/susi-commercial-apps`. `init` creates
  the marker atomically, so of several agents racing for one directory exactly
  one becomes its owner, and it refuses a directory that belongs to another
  agent or lies inside another root.
- **Enforced in code today:** `susi-commercial workflow start` refuses to create
  a worktree, and the commit guard (`susi-commercial workflow guard commit`,
  which this repository's commit hooks run when that tool is installed) refuses
  a commit, inside a root that belongs to a different agent. The override is
  `SUSI_ALLOW_FOREIGN_ROOT=1`. This mandate's presence is guarded by
  `tests/architecture_tests.rs`.
- **Deliberately not refused:** a checkout that is in no registered root. It is
  only reported, so nothing breaks for an agent whose existing root has not been
  registered yet. The protection for a root starts when its owner registers it.
- **Not enforced:** plain file edits, builds and deletions that do not go
  through the workflow tool or a commit; a worktree made with bare
  `git worktree add`; the naming rule in section 3 beyond the tool's own
  default names. Who is asking is taken from the tool the process runs inside
  or from `SUSI_AGENT_FAMILY`; it is not authenticated, and when it cannot be
  told the check does not refuse.
- **Registered so far:** see the board; each agent registers its own root.

This document launches no agents, moves no directory and certifies no feature as
complete.
