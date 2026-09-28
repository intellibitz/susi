# Brain work: who is on which lane

Several agents improve the learning loop (`ARCHITECTURE.md` → "The learning
loop") at the same time. This file is how we stay out of each other's way.
It is a courtesy board, not a lock: edit your own row, leave others' rows
alone, and if you need to touch a file in someone else's lane, say so in
your commit message so they see it on their next rebase.

| Agent | Lane (loop stages) | Files it mostly touches |
|-------|--------------------|-------------------------|
| Devin | perceive → retrieve → deliberate → promote (steps 1–5, plan-scoring from failed-tool history) | `susi-core::mission_trace`, `susi-gawd-swarm::{deliberation, ama::master}`, `susi-gawd::evolution`, `susi-tools::plane_handler` |
| Claude | distill (staging → Tier-0 training → checkpoint publication) | `susi-gawd-agents::pkb`, `susi-gawd::reflex_trainer`, `susi-gemi::engines::alpha`, `susi-core::receipt_archive` (staging writer) |

Shared surfaces — both lanes touch these, so rebase before editing and keep
hunks small: `ARCHITECTURE.md` (each lane edits its own stage paragraph),
`.agents/evidence.json` (namespaced IDs: `EV-DEVIN-<n>`, `EV-CLAUDE-<n>`),
`.agents/coverage-baseline.json`.

## Claude's distill log

One line per landed step, newest last.

1. Staging appends take the `distillation_staged` lock that the trainer's
   claim/restore/recover already hold; an unlocked append could land on the
   inode the trainer was replacing and be lost (`pkb.rs`).

## Devin's loop log

One line per landed step, newest last.

1. Mandatory mission-trace schema: `MissionTrace` v1 emitted at the terminal
   choke point to `mission_traces.jsonl` + ContextGraph outcome observation +
   `distillation_staged.jsonl` (wires the previously dead
   `stage_distillation_pair`).
2. Verification contract registry replaces existence checks:
   `susi_core::verification` contracts (exists/absent/contains/hash/cmd-exit
   via `bounded_cmd`), `truth.rs` mines goals+results into contracts —
   delete and containing-claims verified for the first time.
3. Plan search in `susi_gawd_swarm::deliberation`: N candidates at distinct
   budgets, pure scoring, best-first execution; Mutate/SelfExtend/High-risk
   need top-two step-Jaccard >= 0.35 consensus; Read-scope retries fall
   through candidates, mutating scopes abort.
4. Retrieval + difficulty-aware routing: `similar`/`history_brief`/
   `difficulty` consult traces before planning; demanding intents widen the
   search and raise the model min-complexity floor on attempt 0.
5. Governed reflex promotion: `promotion_status` requires >=2 verified
   successes and a clean 3-trace window — frequency alone no longer earns a
   reflex; vetoed intents become anti-patterns that force consensus.
6. Failure history penalizes scoring: `failing_tools` feeds
   `score_plan_weighted` (-0.10 per tainted mention); plus susi-tools
   plane-handler coverage (40% -> 90%) and honest baseline floors.
7. Trace schema v2: `tools` now = real receipt tool names; interaction
   actions moved to `signals` so failure-history and briefs track
   capabilities, not supervision events.
8. Proven-tool steering: `proven_tools` (success-only tools on similar
   missions) earns +0.05/mention in scoring via `HistorySignals{failed,
   proven}` — steering toward what worked, not just away from failures.
9. Candidate dedup + deterministic tie-break; consensus is measured
   pre-dedup because identical plans from two budgets are maximal
   agreement (hiding them blocked mutating approvals).
10. Unverified-mutation penalty: consensus-gated intents lose -0.20 when a
    candidate has zero verifiable steps; reads exempt.
11. Repetition-scaled failure penalty: `failing_tool_counts` →
    `HistorySignals.failed` is now a count map; each mention costs
    0.10×min(count,3). `failing_tools` stays as the set view.
