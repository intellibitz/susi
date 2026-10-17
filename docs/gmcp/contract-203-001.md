# Contract GMCP-203-001-01 — Normative requirements ledger and standards drift

Status: **contract pinned** (requirement GMCP-203-001-01, vector VC-203-001)
Baseline date: **2026-10-17**

## Normative clause

GMCP conformance is defined exclusively by the machine-readable ledger at
`.agents/gmcp/requirements-ledger.json` over the pinned upstream corpus in
`.agents/gmcp/spec/`. Every applicable MUST, MUST NOT, SHOULD, SHOULD NOT,
MAY, error, deprecation, security rule, official extension rule, and
published implementation claim of MCP **2025-11-25** and **2026-07-28**
appears as exactly one ledger row per keyword-bearing source line. A rule
that is not in the ledger does not exist for qualification purposes, and a
row that exists is never deleted — only re-dispositioned with recorded
reason.

Enforcement is fail-closed in both directions:

- `scripts/check-gmcp-ledger.py` (run by `gmcp_req_203_001_01_contract`
  hermetically, no network) verifies the vendored corpus is byte-identical
  to `.agents/gmcp/sources.json`, deterministically re-extracts every clause,
  and requires the committed ledger to equal the extracted set with complete
  classification;
- `scripts/check-gmcp-ledger.py --drift` (CI/nightly, network) refetches each
  pinned upstream file and fails on any byte change until the corpus is
  re-vendored and every resulting row is classified — an unclassified change
  invalidates the baseline rather than silently passing.

## Pinned sources

All normative content is vendored verbatim under `.agents/gmcp/spec/`; every
file's sha256 and size are recorded in `.agents/gmcp/sources.json`.

| Authority | Pin | Contents |
|---|---|---|
| modelcontextprotocol/modelcontextprotocol | commit `86f6e0c675afea9a8f233e09b6faf566b126086c` (2026-10-15) | `docs/specification/{2025-11-25,2026-07-28}/**` (all `.mdx`), `docs/docs/<rev>/tutorials/security/**`, `schema/<rev>/{schema.ts,schema.json,schema.mdx}` |
| modelcontextprotocol/ext-tasks | commit `899c1bbc869567670093face3591b39c28ce3219` | `specification/2026-07-28/tasks.md` |
| modelcontextprotocol/ext-apps | commit `82221c0c8ce7661efa6771c9d461511b1650495f` | `specification/2026-01-26/apps.mdx` |
| modelcontextprotocol/ext-auth | commit `fb374c7db2b34f18ca9183882e0beecdf661892b` | `specification/stable/enterprise-managed-authorization.mdx` |
| modelcontextprotocol/registry | commit `11b7b0e274d518b6f449c5ee3518a6151d9848b9` | `docs/reference/api/*`, `docs/reference/server-json/*` |
| susi (this repo) | file hash of `docs/gmcp-roadmap.md` | the "Audited starting point" claims — our own published implementation claims, tracked as `claim` rows |

Named but non-vendored authorities (recorded under `external_authorities` in
`sources.json`): the official conformance suite (code, pinned at commit
`c37eec888e1c6ff140af79987a40008548b7cc5f`, consumed by VC-203-045), the OWASP
MCP cheat sheet and Top 10 (threat-model inputs, VC-203-031), the client
feature matrix (VC-203-017), and Inspector guidance (VC-203-044). These guide
implementation but carry no ledger rows; a mirror, blog, or SDK behavior can
never override vendored normative text.

## Canonical data model

`susi/gmcp-requirements/v1`. Each row:

```json
{
  "id": "MCP-2026-0728-basic-index-0047",
  "revision": "2026-07-28",
  "categories": ["behavior"],
  "level": "must_not",
  "source": {
    "repository": "modelcontextprotocol",
    "document": "docs/specification/2026-07-28/basic/index.mdx",
    "sha256": "<pinned file hash>",
    "line": 47
  },
  "clause": "<verbatim source line>",
  "disposition": {"status": "modern|adapter|optional", "reason": "..."},
  "trace": {
    "vector": "VC-203-0NN",
    "owner": "<crate or artifact owner>",
    "production_path": "<file or dir>",
    "positive_test": "tests/gmcp_requirements.rs::gmcp_req_203_NNN_03_integration",
    "negative_test": "tests/gmcp_requirements.rs::gmcp_req_203_NNN_04_adversarial",
    "evidence": ".agents/gmcp/evidence/VC-203-0NN.jsonl"
  }
}
```

- `level` is the strongest RFC-2119 keyword in the clause
  (`shall`/`required`→`must`, `recommended`→`should`, `optional`→`may`).
- `categories` ⊆ `{behavior, error, deprecation, security, extension,
  registry, claim}`; every row has at least one.
- `id` is `TAG-<doc-slug>-<line>`; tags: `MCP-2025-1125`, `MCP-2026-0728`,
  `EXT-TASKS`, `EXT-APPS`, `EXT-AUTH`, `REG`, `CLAIM`.
- `trace.positive_test`/`negative_test` name the owning vector's integration
  and adversarial acceptance tests — the same names the queued
  `T-CODEXGMCPROADMAP-*` tasks are gated on, so a row's proof obligation is
  already bound to a scheduled test.
- `trace.evidence` is the vector's append-only JSONL evidence artifact
  (convention in `.agents/gmcp/evidence/README.md`).

## Behavior table (baseline)

| Class | Rows | Rule |
|---|---|---|
| modern (`2026-07-28` + extensions + registry + claims) | 887 | implement on the shipped production path |
| adapter (`2025-11-25` + deprecated) | 577 | serve only through the version adapter, with downgrade telemetry and removal policy |
| optional (MAY/OPTIONAL) | 249 | conformant either way; the owning vector records the choice in evidence |

Totals: **1,713 rows** over 72 pinned documents — 943 on `2026-07-28`, 641 on
`2025-11-25`, 194 extension, 8 published claims. Categories present:
`behavior` 1,286, `security` 163, `extension` 194, `deprecation` 34,
`error` 32, `registry` 2, `claim` 8.

## Compatibility and deprecation decisions

1. `2025-11-25` is the compatibility baseline: its rules are honored only
   through an explicit version adapter, isolated from the modern path, with
   downgrade telemetry and a removal policy (per the roadmap's legacy
   isolation mandate).
2. `2026-07-28` is the modern target: every non-deprecated rule is a
   `modern` disposition.
3. Anything the modern spec marks deprecated maps to `adapter`, even when it
   appears in a `2026-07-28` document.
4. MAY/OPTIONAL clauses are `optional` in both revisions: conformance never
   requires them, and the owning vector records implement/omit in evidence.
5. Drafts (`specification/draft/*`, `docs/specification/draft/*`,
   `ext-auth`'s draft OAuth client credentials) are explicitly excluded from
   the pinned corpus until promoted — drift detection watches only pinned,
   stable sources.
6. The registry reference documents are descriptive API docs rather than
   RFC-2119 text (only 2 normative rows extract); VC-203-035's own contract
   derives operational obligations from them — this is a recorded scoping
   decision, not a gap.

## Threat model

| Threat | Control |
|---|---|
| Silent upstream spec drift redefining "complete" | `--drift` refetches every pinned file and fails closed on any hash change |
| Unclassified clause slipping in | checker requires the committed ledger to equal the deterministic extraction of the corpus — a missing or edited row fails |
| Tampered vendored evidence | every vendored file's sha256/size is verified against `sources.json` before extraction |
| Scope creep via mirrors/blogs/SDKs | non-vendored authorities are advisory only and enumerated in `external_authorities` |
| Retroactive weakening of a requirement | rows are never deleted; re-disposition requires a recorded reason and passes through the same checker |
| Claim inflation | `CLAIM-*` rows are verbatim-excerpted from `docs/gmcp-roadmap.md` and pinned by file hash — a marketing edit without reclassification fails |

## SLOs and resource budgets

- Ledger coverage: 100% of extracted clauses classified; 0 unclassified rows.
- `scripts/check-gmcp-ledger.py` completes in < 60 s on the dev gate host
  (it is pure file hashing + regex over ~2.7 MB).
- `requirements-ledger.json` stays under 10 MB at this baseline (currently
  ~2 MB); growth is bounded by upstream spec size.
- `--drift` covers all 72 pinned files in one CI job (< 5 min network-bound).
- Evidence files are append-only JSONL; a vector's evidence budget is
  unbounded in count but bounded in per-record size (≤ 64 KiB per line).

## Fixtures

`.agents/gmcp/fixtures/` holds deterministic JSON fixtures, split
`positive/` (must accept/produce) and `negative/` (must reject), each naming
the ledger row it exercises. Seeds shipped with this contract:
`positive/server-discover-2026-07-28.json` (valid `server/discover` on the
modern path) and `negative/request-null-id.json` (violates
`MCP-2026-0728-basic-index-0047`). Fixtures are immutable once referenced.

## Evidence locations

`.agents/gmcp/evidence/VC-203-<NNN>.jsonl` is the canonical, append-only
evidence artifact per vector (see `evidence/README.md`). The ledger, the
vendored corpus, `sources.json`, and this contract are themselves immutable
evidence — committed, hash-pinned, and verified every gate run.

## Ambiguities resolved

- **Clause granularity**: one row per keyword-bearing source line; wrapped
  sentences contribute one row per keyword line; fenced code is excluded
  (examples carry no obligations).
- **Keywords inside prose** (e.g. "a `null` id MUST NOT…" inside tables):
  extracted identically; tables are normative text.
- **`schema.ts`/`schema.json` comments**: extracted like `.mdx` (doc-comment
  normative text is part of the schema contract).
- **`error` rows**: categorized when the clause names a numeric JSON-RPC
  error code (`-32xxx`) or an error code rule; VC-203-002 owns them.
- **Extension vs core**: Tasks (ext-tasks `2026-07-28`), Apps
  (`2026-01-26`), and enterprise authorization (ext-auth stable) are official
  extensions → `extension` category, `modern` disposition, vectors
  VC-203-014/015/027.
- **Own claims**: implementation claims Susi publishes about itself
  (`docs/gmcp-roadmap.md`) are `claim` rows with `must` level and `modern`
  disposition — claims are binding.
- **Test binding**: `positive_test`/`negative_test` name scheduled work-package
  tests, not yet-implemented ones; the binding is by naming convention and is
  satisfied when VC-203-0NN's queue lands.

## Operations and incident runbook

### Inspectable state

- `susi mcp ledger` — the production health check. Emits a bounded JSON
  report (`rows`, `documents`, `revisions`, `dispositions`, `categories`,
  `vectors`, `verified`) and exits non-zero on any verification failure.
  The same payload is served by the `ledger_verify` tool on every dispatch
  surface (direct call, registry/plane-bus, daemon, served stdio), so a
  dashboard or operator sees one canonical shape.
- `susi mcp ledger --drift` — live upstream-drift check. Refetches every
  pinned source (bounded: 15 s per request, 300 s total, 64 MiB per body)
  and emits `checked`/`matched`/`findings[]` (each `document`, `kind`
  `hash_drift|unfetchable`, `detail`). Non-zero exit on any finding.
- `susi mcp ledger --drift --refetched <dir>` — deterministic variant:
  compares a pre-fetched corpus directory (keyed by manifest `document`
  name) instead of the network. Use it in air-gapped gates and in tests.
- `scripts/check-gmcp-ledger.py` — the authoring checker for
  authoring-time roles (`--write` regenerates the ledger; `--drift` is the
  Python live check). The shipped Rust path above is what deployments run.

### Diagnostics

Every failure carries a typed class: `parse` (malformed artifact),
`drift` (vendored bytes disagree with the pinned hash), `confinement`
(a `document` name escaping the pinned tree), `bounds` (artifact over the
size/row limits), `unfetchable` (upstream unreachable or response
oversized), `hash_drift` (upstream changed). The CLI surfaces the raw
`EaiError` text; the JSON report's `findings[].kind` carries the class for
drift results.

### Incident triage

| Signal | Meaning | Action |
|---|---|---|
| `susi mcp ledger` exits non-zero naming a vendored source | local corpus tampered or stale vs `sources.json` | restore the file from git; if `sources.json` itself changed, treat as unauthorized re-pin — see rollback |
| non-zero naming a ledger row / verbatim clause | `requirements-ledger.json` edited without regenerating | regenerate via `scripts/check-gmcp-ledger.py --write` on a task branch, or revert |
| `--drift` `hash_drift` finding | the pinned upstream file changed upstream | deliberate re-pin required — see migration |
| `--drift` `unfetchable` finding | network outage, upstream deleted, or body over bound | retry; persistent failure means the pin's source is gone — recover from git history or the vendored copy, then re-pin |
| `unfetchable` on `--refetched` | the comparison dir lacks the file | re-fetch the dir; the check fails closed rather than skipping |

### Migration (re-pinning an upstream standard)

1. Claim a task; bump the pinned `commit` in `sources.json` (upstream pin
   table) to the new revision.
2. Refresh the vendored corpus from the new commit.
3. `scripts/check-gmcp-ledger.py --write` regenerates the ledger from the
   new corpus; review the row-level diff — new clauses must be
   dispositioned, dropped clauses keep their rows as re-dispositioned.
4. `susi mcp ledger` and the gate must pass before the re-pin merges.

### Rollback

The whole corpus, manifest, ledger, and fixtures are committed artifacts —
rollback is `git revert` of the re-pin commit. Because rows are never
deleted and every artifact is hash-pinned, a rollback restores the exact
prior pinned state; verification and drift checks then pass on the old pin
with no data migration.
