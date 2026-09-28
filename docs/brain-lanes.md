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
2. Tier-0 checkpoints publish only if a candidate fit does not regress
   held-out accuracy against the active checkpoint; a regression restores
   the claim instead of shipping worse weights (`alpha.rs`, EV-CLAUDE-002).
3. Failed missions no longer train the reflex model: the trainer skips any
   sample whose recorded outcome is not a success, and mission reports stop
   staging failures (`alpha.rs`, EV-CLAUDE-003; one guard in Devin's `ama/report.rs`).
4. Unknown words hash with FNV-1a, not a byte sum that made every anagram
   one feature (reflex classifier and fleet recruitment; EV-CLAUDE-004).
5. Replay set: every training cycle rehearses the last 2048 distinct
   trained intents, so a new batch no longer overwrites what earlier ones
   taught; the held-out gate now also catches forgetting (EV-CLAUDE-005).
6. The classifier gets its own features (stopwords dropped, every word
   equal weight): a leading "write" no longer turns "write a poem" into
   `write_file` at 0.84 confidence; fleet keeps its projection (EV-CLAUDE-006).
7. Support gate: a reflex is served only when the prompt is near (cosine
   >= 0.6) something the model trained on — the classifier has no abstain
   class, so confidence alone cannot refuse the unfamiliar (EV-CLAUDE-007).
8. Distillation log + back-off: every training cycle's outcome lands in
   `.susi/distillation_log.jsonl`; a held-back claim waits for `threshold`
   new samples instead of being refit on every mission (EV-CLAUDE-008).

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
