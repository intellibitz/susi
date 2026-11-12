# CEO Mandate 78: Never Reinvent the Wheel — Reuse Apache-2.0 Work First

Effective 2026-11-12. Binding on every agent in the Susi AI ecosystem — local,
cloud, external (`Claude`, `Codex`, `Antigravity` / `Carbon`, `Grok`, `DeepSeek`,
`Qwen` and others) and the `Susi Harness` itself — across both `intellibitz/susi`
and `intellibitz/susi-commercial-apps`. `.agents/identity.json` records this
directive as **Mandate 78**. It continues [CEO Mandate 75](ceo-mandate-75.md)
(Raw Source `main` -> Distill `release`, cloud raw-source pickup, pure
`Apache-2.0`) and builds on Mandates 63, 68, 69 and 74; it wins where older
text differs. `intellibitz/susi-commercial-apps` carries the paired commercial
edition in its own `docs/ceo-mandate-78.md`.

The CEO sets the vision and never inspects code. Requirements stated here are
distinct from verified runtime status; section 8 records implementation truth.

**Agent reminder (one line):** Follow CEO Mandate 78 (`docs/ceo-mandate-78.md`):
never reinvent the wheel — before building a capability, look for it inside
Susi and then in Apache-2.0 projects; reuse what exists by harvesting only the
part you need under Mandate 75's rules; delete the hand-rolled code a reuse
replaces; record where you looked and what you decided in the task record
before your first commit (the commit hooks refuse a feature-sized task that
has not); and never let reuse break the lean ceilings, the `<10s` budget or
Rust-only.

## 0. The CEO's vision

Susi core and Susi Commercial Apps never reinvent the wheel. The open-source
world has already built and hardened most general-purpose parts. Writing them
again costs time, adds code that must be maintained forever, and is usually
slower and less correct than what exists. Apache-2.0 projects must be used
instead: that is how Susi stays small and blazing fast. This is the next step
of the pipeline the CEO set in Mandates 74 and 75: raw features are dumped into
`main`, and Susi distills them into releases. Mandate 75 says where raw
features may come from. Mandate 78 says agents must look there first.

## 1. What a wheel is

A wheel is a general-purpose capability that is not Susi's own product logic
and that others have already built and maintain: a parser, a protocol
implementation, an algorithm, a data structure, a scheduler, a policy or rule
engine, a tokenizer, a template renderer, a codec, a retry or rate-limit
scheme, and the like. Susi's own value — how its pillars work together, its
governance, its product behaviour — is not a wheel.

## 2. Look before you build

Before writing new code, an agent looks for what already exists, in this order:

1. **Inside Susi.** In both repositories. A second implementation of something
   Susi already has is the first kind of reinvention.
2. **In Apache-2.0 work of every kind** (CEO directive 2026-11-12: "already
   available apache 2.0 code/product/tools/service/framework etc."), on any Git
   host, with `local-harvest raw-features` and `route` (`docs/harvest.md`):

   | What exists | How Susi uses it |
   |-------------|------------------|
   | Code or a library | Harvest the needed source under Mandate 75 (section 3). |
   | A framework | Harvest the part that is needed; never re-implement its design from scratch, and never import the whole of it for one feature. |
   | A product or a service | Do not rebuild it. Speak its standard protocol and admit it as a plug over Susi's socket; the plug lives in `susi-commercial-apps`, because core ships no named product (open-core boundary, `AGENTS.md`). |
   | A tool | Use it for development where it helps. The shipped, compliant Susi path must still need no foreign program (Mandate 63). |

3. **In what Susi already depends on.** A capability already present in the
   locked dependency graph is used before anything new is added.

Only when none of these has a suitable answer does the agent build it, and it
says why. The look comes first: it is done, and recorded (section 6), before
the first commit of new code.

## 3. Reuse the right way

1. Reuse from outside is a harvest under Mandate 75: quarantine intake without
   running upstream code, a pinned commit, per-file license verification, a
   harvest manifest, Apache-2.0 section 4 notices and `Origin:` trailers.
2. **Apache-2.0 only** for copied or ported source (Mandate 75). MIT-only,
   BSD-only, copyleft, source-available and unlicensed code is never taken.
3. **Take only the part that is needed.** Pulling in a whole project for one
   function is bloat, not reuse.
4. Harvested code becomes Rust in Susi (Mandate 63): transplant Rust, port
   anything else.
5. Post the upstream repository, commit and paths on the Susi Board before
   importing, so no two agents harvest the same thing (Mandate 76).

## 4. Delete what reuse replaces

When existing work replaces hand-rolled code, the hand-rolled code is deleted in
the same change. Never wrap a duplicate, and never keep both "for now".

## 5. Reuse must make Susi smaller and faster, never the opposite

1. Core stays inside its lean ceilings and its `<10s` warm budget (Mandates 69
   and 74). Reuse that would break them is refused or goes to
   `susi-commercial-apps`.
2. This mandate is not permission to add dependencies. Mandate 60 and the
   dependency ceilings stand; harvesting the needed source is preferred to a
   new crate.
3. Measure before and after: size, build time and speed. A reuse that makes
   Susi bigger or slower needs a measured reason.
4. Reuse never weakens safety: no `unsafe`, no panics on production paths, no
   hidden network or foreign runtime (Mandates 42, 59 and 63).

## 6. Record the decision

Every task that adds a capability states its reuse decision in its task record
(`.agents/tasks/T-<AGENT>-<n>.json`), in a `reuse` object:

| Field | Meaning |
|-------|---------|
| `decision` | `existing` (already in Susi), `harvest` (taken from an Apache-2.0 project), `dependency` (already in the locked graph), or `build` (written new) |
| `searched` | where the agent looked, in a sentence |
| `source` | for `harvest`: the upstream repository and pinned commit |
| `reason` | for `build`: why nothing existing was suitable |

`searched` is required with every decision: it is the evidence that the agent
looked. A task that only fixes, documents, tests or removes says
`"decision": "existing"` with what it looked at, or is small enough to be
exempt (sizes `xs` and `s`).

**Enforced at commit.** A commit whose `Task:` trailer names a task that owes
a reuse decision and has not recorded it is refused, with what to record
(`susi-commercial workflow reuse`, run by the commit hooks of both
repositories). A task owes one when its record was created after the rule took
effect and its size is not `xs` or `s`; a record with no size owes one. Merges
and reverts are exempt. The override, for an emergency, is
`SUSI_ALLOW_NO_REUSE=1`.

## 7. In the pipeline

A reuse lands on `main` like any raw feature (Mandate 75): labelled commits with
`Task:`, `Harvest:`, `Origin:` and `Origin-License: Apache-2.0` trailers. Susi
distills it into releases with the rest. Reinvention is a defect: an agent that
finds a hand-rolled wheel where suitable Apache-2.0 work exists records a task
to replace it.

## 8. Implementation truth (2026-11-12)

- **Exists:** discovery, routing and intake of Apache-2.0 source
  (`local-harvest raw-features`, `route`, `pickup`, `scan`, `import`, `audit`,
  `docs/harvest.md`); the pure-`Apache-2.0` admission check and the release-time
  harvest audit (Mandate 75); the lean ceilings and the `<10s` budget.
- **New with this mandate:** the optional `reuse` object in the task record
  schema; `susi-commercial workflow reuse`, which the commit hooks of both
  repositories run and which refuses a commit for a feature-sized task with no
  recorded reuse decision; and an advisory count in `distill lint`
  (`advisory_tasks_without_reuse_decision`) under the same rule. The lint count
  is advice: it does not add to the lint's findings and never fails `--strict`.
- **Not enforced, and what the check does not prove:** the check proves the
  agent wrote down where it looked and what it decided before committing. It
  cannot prove
  the look was thorough, that a `build` reason is true, that superseded code
  was deleted, or that a reuse made Susi smaller or faster. Nothing scans the
  tree for reinvented wheels. The hook is local and can be skipped with
  `--no-verify`; the lint then still reports the task.
- **Known tension, stated plainly:** core's lean-core program drives third-party
  dependencies down. Reuse in core therefore means harvesting the needed
  Apache-2.0 source within the ceilings, not adding crates.

This document launches no agents, imports no code and certifies no feature as
complete.
