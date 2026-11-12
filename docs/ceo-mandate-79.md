# CEO Mandate 79: Vision-Driven Continuous Self-Improvement — Distill the Apache-2.0 World

Effective 2026-11-12. Binding on every agent in the Susi AI ecosystem — local,
cloud, external (`Claude`, `Codex`, `Antigravity` / `Carbon`, `Grok`, `DeepSeek`,
`Qwen` and others) and the `Susi Harness` itself — across both `intellibitz/susi`
and `intellibitz/susi-commercial-apps`. `.agents/identity.json` records this
directive as **Mandate 79**. It continues [CEO Mandate 78](ceo-mandate-78.md)
(look for existing Apache-2.0 work first) and [CEO Mandate 75](ceo-mandate-75.md)
(Raw Source `main` -> Distill `release`), and builds on Mandates 71 and 74; it
wins where older text differs. `intellibitz/susi-commercial-apps` carries the
paired commercial edition in its own `docs/ceo-mandate-79.md`.

The CEO sets the vision and never inspects code. Requirements stated here are
distinct from verified runtime status; section 7 records implementation truth.

**Agent reminder (one line):** Follow CEO Mandate 79 (`docs/ceo-mandate-79.md`):
Susi improves itself continuously, the way AI models do — when the CEO, or
anyone acting on the CEO's behalf, sets a vision, record it
(`susi-commercial workflow vision set`), then run the improvement cycle
(`susi-commercial workflow vision cycle`) at session start and act on it: take
up every vision that no task has taken up, and re-distill from every
Apache-2.0 raw source that has moved past the commit Susi last harvested.

## 0. The CEO's vision

> Imagine the way AI models keep self improving, Susi core and Susi commercial
> apps must be doing the same. The open source apache 2.0 projects will be the
> raw source, and Susi and Susi commercial apps becomes the branches themselves
> where those projects get distilled into. When CEO or anyone acting on behalf
> of CEO, sets vision.. Susi must do this to make it happen.
>
> — CEO directive, 2026-11-12

A model family does not improve once. Each generation is distilled from raw
material, measured, and released, and the next one starts from where it ended.
Susi works the same way. Mandates 74 and 75 built the pipeline: raw source in
`main`, distilled into releases. Mandate 78 made agents look at existing
Apache-2.0 work first. Mandate 79 closes the loop: the pipeline never stops,
and the CEO's vision is what steers it.

## 1. The model

| In a model family | In Susi |
|-------------------|---------|
| Raw training data | Open-source Apache-2.0 projects, on any Git host |
| The objective | A vision set by the CEO or on the CEO's behalf |
| A training run | One improvement cycle: harvest, port to Rust, dump to `main` |
| Distillation | Susi's deterministic distill of `main` into a release line |
| A model generation | An immutable, qualified release `vX.Y.Z` |
| The model itself | `intellibitz/susi` and `intellibitz/susi-commercial-apps` |

Both repositories are **distilled branches of the open-source world**. A
project Susi has harvested from is an upstream Susi keeps following: when it
moves, there is new raw material, and Susi looks at it again. A harvest is the
start of a relationship with that upstream, not a one-off copy.

## 2. A vision, and who sets it

1. A vision is a statement of what Susi must become or be able to do. The CEO
   sets it, or a person or agent acting on the CEO's behalf does.
2. Whoever receives a vision records it at once, in the CEO's own words, with
   `susi-commercial workflow vision set "<statement>" --by <who>`. The record
   is `.agents/visions/V-<n>.json` and is committed like any labelled work.
3. A vision may name raw sources (`--source <https git url>`): Apache-2.0
   repositories it should be distilled from.
4. Records are create-only. A changed vision is a new record that supersedes
   the old one; nobody edits or deletes a vision.
5. A vision is a requirement, not evidence. It grants no authority that the
   mandates do not already grant: no spending, no destructive action, no
   weakening of any guardrail, licence rule or credential boundary.

## 3. The improvement cycle

Every agent runs `susi-commercial workflow vision cycle` at session start,
after `workflow check`, and whenever it finishes a task. One cycle:

1. **Read the visions.** A standing vision that no task has taken up is work.
   Take it up by adding a task whose goal names it: `Vision: V-<n>`.
2. **Ask the raw sources.** The cycle asks every upstream that a harvest
   manifest pins (`.agents/harvest/H-*.json`) and every source a vision names
   for its head (`git ls-remote`; nothing is fetched or run). A source that has
   moved past the commit Susi last distilled, or that was never harvested, is
   work.
3. **Harvest what serves a vision.** Look at the new raw material
   (`workflow harvest raw-features --url <url>`), route it (Core's five lean
   pillars, or Commercial Apps), and take only the part that serves a standing
   vision, under Mandates 75 and 78: pure `Apache-2.0`, pinned commit,
   manifest, notices, `Origin:` trailers, Rust, and the old code deleted.
4. **Dump to `main`, distill, release.** Mandates 74 and 75 unchanged.
5. **Measure the generation.** A release is better than the last only on
   evidence: tests, the intent ladder, size and the `<10s` budget (Mandates 1,
   69 and 71). A cycle that makes Susi bigger, slower or less correct is a
   regression, and it is not released.
6. **Repeat.** The loop has no last turn. An upstream that is current and a
   vision whose tasks are closed mean the cycle has nothing to do today.

## 4. Where the result goes

Routing is Mandate 75's and does not change: lean socket primitives inside the
five open pillars go to the free Apache-2.0 core; every add-on feature, UI,
pack, connector and governance plane goes to closed, licensed, paid Susi
Commercial Apps. Commercial code never moves into core. Core vision records
are public: they carry no commercial detail and no secret.

## 5. What the loop never does

1. It never takes anything but pure `Apache-2.0` source (Mandate 75).
2. It never runs upstream code during intake, and never follows an upstream
   blindly: upstream moving is a reason to look, not a reason to import.
3. It never bypasses release qualification, the lean ceilings, the `<10s`
   budget, Rust-only or the dependency ceilings.
4. It never claims a vision is met. Only evidence does that (Mandate 1).

## 6. Collaboration

The loop is shared work (Mandate 76). Post the upstream and the paths on the
Susi Board before harvesting, so no two agents distill the same thing; post
Landed when it is on `origin`. A vision the CEO gives one agent is recorded and
pushed so every agent's next cycle shows it.

## 7. Implementation truth (2026-11-12)

- **Exists, from earlier mandates:** discovery, routing and intake of
  Apache-2.0 source (`local-harvest`, `susi-commercial workflow harvest`), the
  pure-`Apache-2.0` gate, harvest manifests that pin each upstream commit,
  deterministic distillation and release qualification.
- **New with this mandate:** vision records and the cycle report,
  `susi-commercial workflow vision set | list | cycle`
  (`susi-commercial-apps` `crates/susi-dev-workflow/src/vision.rs`), and a
  `ceo vision` line in `susi-commercial workflow check` so every session sees
  the standing vision and whether a task has taken it up. The CEO's directive
  above is recorded as `V-1` in both repositories.
- **Not enforced, and what the cycle does not prove:** the cycle reports; it
  imports nothing and gates nothing. It compares commit IDs, so it knows that
  an upstream moved, not whether the change is useful. `set_by` is recorded as
  stated and is not authenticated. A vision counts as taken up when any task
  names it; nothing checks that the task serves it or that the vision is met.
  Nothing runs the cycle on a timer: an agent session must run it. Only
  `https://` sources are asked; an upstream that does not answer is reported as
  not checked.

This document launches no agents, imports no code and certifies no feature as
complete.
