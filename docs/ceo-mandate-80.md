# CEO Mandate 80: Step 1, three core pillars and synchronized delivery

> **History, superseded.** This record is kept for its reasoning only. The
> rules that bind agents today are in [`AGENTS.md`](../AGENTS.md), the one
> active contract; where this record differs, `AGENTS.md` wins.

Effective 2026-11-15. This directive supersedes only the older four/five-pillar
taxonomy and delivery cadence, not release, safety, license or ownership rules.

Susi core has exactly three architectural pillars:
1. **GAWD**: agent coordination and execution, including A2A, ACP, and Susi
   Harness with parallel multi-agent workflow.
2. **GEMI**: model inference routing.
3. **GMCP**: MCP protocol and governed tool plane.

ACP and Susi Harness remain supported GAWD capabilities, not extra pillars.
Existing crate names, compatibility imports, protocol endpoints and the five-port
host contract remain valid. A transport count is not an architectural pillar count.
Commercial applications, UI, enterprise policies and product integrations remain
in the private, licensed commercial repository.

During active work, inspect ready scoped work for integration every ten minutes.
Reconcile fresh remote main and ownership before integrating. Preserve peer work;
do not force an empty, conflicted or incomplete merge to meet the clock. Record
the outcome and blocker when no safe change is ready. Main remains unqualified
development; exact-tree release qualification, immutable tags, health checks and
rollback are unchanged. Cadence requirements are not evidence that a timer ran.

Update source, regression tests, constitution, mandates and current docs together.
The user-facing goal for both repositories is fast, dynamic, configurable, flexible,
zero-required-configuration, self-correcting and self-improving behavior. Treat
these as measured acceptance targets, not accomplished marketing claims.
This remains Step 1. Preserve its existing cold/warm timing, live delivery,
promotion and rollback evidence requirements; do not move unfinished Step 1 work
into later steps or declare general intelligence or complete bug freedom.

## Implementation scope

This change adds a three-value core taxonomy and normalizes legacy
ACP/harness harvest-routing inputs to GAWD, retaining protocol compatibility and
commercial refusal precedence. Test outcomes are recorded separately.
This document does not establish Step 1 completion or waive ownership checks.

## Implementation truth

- **Held by code and tests:** `crates/susi-core/src/pillars.rs` defines exactly
  three pillars (`CorePillar::ALL`) and maps the legacy `a2a`, `acp`,
  `harness` and `swarm` capability names to GAWD
  (`exactly_three_canonical_pillars`,
  `gawd_owns_a2a_acp_and_parallel_harness_capabilities`). `tests/three_pillars_governance.rs` keeps
  the current guidance (`AGENTS.md`, `README.md` and Mandates 74, 75, 79 and
  81) naming three core pillars with no stale five-pillar wording, and keeps
  this document's capability, cadence and release boundaries.
- **Not enforced:** the ten-minute integration review is a cadence agents keep;
  no timer runs it and nothing refuses work when it is missed. The acceptance
  targets (fast, zero-configuration, self-correcting, self-improving) are goals
  to measure, not checks a tool runs. Step 1 is not declared complete.
