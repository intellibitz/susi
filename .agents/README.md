# Agent governance ledgers

These files are **not end-user documentation**. They are machine-readable
source of truth for the substrate genome, compiled into the binary at build
time (`susi-gawd-agents` `build.rs` → `AlphaSelf`).

| File | Schema | Role |
|------|--------|------|
| `identity.json` | `susi/identity/v1` | Constitutional mandates, topology, protocols |
| `roadmap.json` | `susi/roadmap/v1` | Evolutionary vectors |
| `evidence.json` | `susi/evidence/v1` | Append-only audit / mission ledger (`entries[]`) |
| `schemas/*.schema.json` | JSON Schema 2020-12 | Shape contracts |

## RSI swarm AI operating-layer backlog

`roadmap.json` preserves the two `VC-200-*` vectors and adds exactly 100
planned tasks, `VC-201-001` through `VC-201-100`. Each block of ten covers,
in order: RSI evaluation, RSI execution, swarm execution, federation, local
hosting, cloud hosting, configuration, governance, memory and ecosystem
integration, and operations. This is a multi-release backlog; the existing
`target_version` is not a delivery promise for all 100 tasks.

For these tasks, `vector` names the deliverable, `mastery_target` states the
testable completion criteria, and `progress` records implementation status.
Optional `priority` is P0 (foundation or critical correctness), P1 (delivery),
or P2 (later optimization). `depends_on` lists prerequisite task IDs, not
implemented features; complete their acceptance criteria before accepting
the dependent task. Tasks without dependency edges can proceed independently
subject to their actual code and resource constraints.

Completion requires production wiring, typed failures, bounded resource use,
relevant failure-path tests, and evidence identifying the revision, commands,
and results. Apply AGENTS.md gates and update current-product claims only
after verification. Self-patches build in `target/`, verify on `~/.susi-dev`
(ports 9190-9194), and enter `~/.susi/bin` only via tagged releases promoted
by `scripts/susi-release-sync.sh` under Mandate 48.

"100% better" is an ambition, not a completion percentage or capability claim.
The scorecard task establishes baselines and metrics; the final qualification
task requires measured results and limitations before reporting improvement.

Public host contract and Tier S pillars for humans live in `README.md` (repo
root). When README and `identity.json` disagree, **source code wins** and both
are updated in the same change.

Append evidence by pushing an object onto `evidence.json` → `entries` (never
rewrite history of prior `proof` fields except to correct factual error, with
a new `EV-*` entry noting the correction).
