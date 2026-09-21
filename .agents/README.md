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

Public host contract and Tier S pillars for humans live in `README.md` (repo
root). When README and `identity.json` disagree, **source code wins** and both
are updated in the same change.

Append evidence by pushing an object onto `evidence.json` → `entries` (never
rewrite history of prior `proof` fields except to correct factual error, with
a new `EV-*` entry noting the correction).
