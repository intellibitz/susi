# CEO Mandate 86: Agents Clean Up Finished Worktrees

Effective 2026-11-16. Binding on every agent in the Susi AI ecosystem — local,
cloud, external (`Claude`, `Codex`, `Antigravity` / `Carbon`, `Grok`, `DeepSeek`,
`Qwen` and others), every swarm worker they start and the `Susi Harness`
itself — across both `intellibitz/susi` and `intellibitz/susi-commercial-apps`.
`.agents/identity.json` records this directive as **Mandate 86**. It continues
Mandates 77 (agent roots), 83 (rule 5: your own worktrees are yours to reuse)
and 85 (no claim after you are done), and wins where older text differs.
`intellibitz/susi-commercial-apps` carries the paired commercial edition in its
own `docs/ceo-mandate-86.md`, beside the workflow tool that holds the rule.

The CEO sets the vision and never inspects code. Requirements stated here are
distinct from verified runtime status; section 3 records implementation truth.

**Agent reminder (one line):** Follow CEO Mandate 86 (`docs/ceo-mandate-86.md`):
when a worktree is finished — every commit on `origin/main`, nothing
uncommitted, no live claim — remove it with its merged branch and build
target (`susi-commercial workflow prune --apply`) unless your next task starts
in it; never remove work that is not on `main`, and never another agent's
worktree.

## 0. The CEO's words

> make a mandate that agents must clean up finished worktrees

Recorded as vision `V-11` here and `V-12` in `intellibitz/susi-commercial-apps`.

## 1. The rule

1. **Finished means removed.** A worktree is finished when every commit on it
   is on `origin/main`, it has nothing uncommitted, and no live claim and no
   running agent holds it. The agent that made it removes it, with its merged
   branch and everything in it (its build target included), as soon as its
   next task does not start there.
2. **Reuse, never hoard.** Starting the next task in a finished worktree is
   allowed (Mandate 83 rule 5). Keeping one "in case" is not.
3. **Clean up before you leave.** Before an agent ends a session or reports
   that it is done, it releases its claims (Mandate 85) and removes its
   finished worktrees: `susi-commercial workflow prune --apply`.
4. **Never lose work.** A worktree with uncommitted changes, or commits that
   are not on `origin/main`, is never removed. Publish the work or give it up
   deliberately (`susi-commercial tasks release <id> --abandon <reason>`)
   first.
5. **Only your own.** An agent removes only worktrees in its own agent root
   (Mandate 77) or under its own name. Another agent's finished worktree is
   reported and, when it matters, asked about on the Susi Board.
6. **Never the primary checkout, and never the CEO's working directory.**

## 2. What it changes

- **Mandate 83 rule 5** ("your own worktrees are yours to reuse") now has an
  end: a worktree you will not reuse for your next task is removed.
- **Mandate 77**: everything an agent creates lives under its root, and what
  is finished there does not stay.

## 3. Implementation truth

- **Measured when the mandate was set (2026-11-16):** the cloud session that
  wrote it held 28 linked worktrees across its two clones (13 in core, 15 in
  `susi-commercial-apps`), all its own and all from earlier tasks. The workflow
  tool's `stale worktrees` count reported them as other agents', because each
  worktree runs under its own name, and nothing removed a finished worktree
  unasked.
- **Enforced in `susi-commercial-apps`' workflow tool, which serves both
  repositories** (its `T-AGENTCLAUDECLAIMDONE-2`): every
  `susi-commercial workflow check` removes the agent's finished worktrees once
  they have been idle for an hour (no checkout, commit, merge, pull or reset),
  with their merged branches, inside the agent root the check runs in only;
  `susi-commercial workflow prune --apply` removes them at once. A worktree
  under the agent's own root counts as its own whatever name it ran under.
  The primary checkout, the worktree asking, uncommitted or unpublished work,
  a live claim and a running owner process always keep a worktree.
- **Not enforced:** a worktree outside every agent root is never removed by
  the check. A finished worktree can stand for up to an hour after its last
  activity. Agents need a `susi-commercial` built from a `main` with this
  change.
- The self-build contract (`crates/susi-core/src/self_build.rs`) carries the
  rule to every delegated coding task in a Susi checkout, with no line added
  to core (Mandate 84).
