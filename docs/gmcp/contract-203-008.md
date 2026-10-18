# GMCP contract — VC-203-008: schemas, primitives, content, and metadata model

This contract translates the VC-203-008 mastery target into a normative
clause and behavior table before implementation begins.

> For MCP 2026-07-28 and later every MCP primitive and content variant is
> represented losslessly, including annotations, icons, resource links and
> embedded resources, binary media, extensions, cache scope/TTL, audience
> and priority metadata, and JSON Schema 2020-12 input/output contracts.
> Round-trip fixtures preserve unknown extension fields; schema limits,
> external-reference policy, Unicode, URI and MIME validation, and
> canonical serialization are exhaustively tested.

## Normative clause

VC-203-008 binds the 425 ledger rows traced to this vector: the
`schema/2026-07-28` and `schema/2025-11-25` canonical schema corpora
(JSON, TypeScript, MDX renderings), the `docs/specification` schema and
caching/pagination documents for both revisions, and one published
baseline claim (CLAIM-03). GMCP represents every primitive — tools,
resources, resource templates, prompts, completions, content blocks,
annotations, icons, capabilities — through one canonical data model
(`rmcp::model`, vendored under `susi-vendor-mcp-server`) and guarantees:

- **Lossless representation**: every content variant (`text`, `image`,
  `audio`, `resource_link`, `resource` embedded contents) and every
  metadata carrier (`_meta`, annotations, icons, audience, priority,
  cache hints) serializes and deserializes without dropping or
  inventing fields.
- **Unknown-field preservation**: extension fields not named by the
  canonical schema round-trip verbatim — the protocol's extension
  policy never silently discards a key a future revision may define.
- **Validation**: URI, MIME, Unicode, base64, and JSON Schema 2020-12
  input/output contracts are checked at the boundary, and failures are
  typed, deterministic errors — never silent coercion.
- **Bounded resources**: schema and payload sizes are bounded by the
  VC-203-002 wire budgets; canonical serialization is deterministic.

## Pinned sources

| Document | SHA-256 (short) |
| --- | --- |
| `docs/gmcp-roadmap.md` (baseline CLAIM-03) | `30a6061c099a4a50` |
| `schema/2026-07-28/schema.json` | `ef70b61f99b6d2e5` |
| `schema/2026-07-28/schema.ts` | `742750af0bb8c716` |
| `schema/2026-07-28/schema.mdx` | `6864885fa1ddb19f` |
| `docs/specification/2026-07-28/schema.mdx` | `bdfb232898c79aef` |
| `docs/specification/2026-07-28/server/utilities/caching.mdx` | `ca416e94b40c067d` |
| `docs/specification/2026-07-28/server/utilities/pagination.mdx` | `c4c7b674ae9ce16c` |
| `schema/2025-11-25/schema.json` | `268a5f82ba70fd7e` |
| `schema/2025-11-25/schema.ts` | `e74b56e73b2e37bd` |
| `schema/2025-11-25/schema.mdx` | `e5253113e703a674` |
| `docs/specification/2025-11-25/schema.mdx` | `03c66be1ec2c04c7` |
| `docs/specification/2025-11-25/server/utilities/pagination.mdx` | `81a715102e8da34a` |

Pinned via `.agents/gmcp/sources.json`; drift detection is the shared
`susi mcp ledger --drift --refetched` check. The three renderings of each
revision's schema (`.json`, `.ts`, `.mdx`) are one logical corpus: the
`.json` is the machine-canonical form, `.ts` the type authority, and
`.mdx` the human rendering; the contract binds all three so no rendering
can diverge unobserved.

## Canonical data model

The canonical owner is `crates/susi-gmcp` consuming `rmcp::model`
through the `susi-vendor-mcp-server` facade — never re-declared per
surface:

| Primitive | Canonical type | Notes |
| --- | --- | --- |
| Content blocks | `model::ContentBlock` — `Text`, `Image`, `Audio`, `Resource` (`EmbeddedResource`), `ResourceLink` | `#[serde(tag = "type")]`; `non_exhaustive` |
| Annotations | `model::Annotations` — `audience: Vec<Role>`, `priority: f32 ∈ [0,1]`, `lastModified` | audience/priority metadata pinned |
| Embedded resource | `model::EmbeddedResource` wrapping `ResourceContents::{Text,Blob}` | binary media as base64 `blob` + `mimeType` |
| Icons | `model::Icon` set on tools/resources/prompts/serverInfo | URI or `data:` URI |
| Metadata | `_meta` objects — passthrough maps; extension keys preserved | unknown keys round-trip |
| Tool schemas | `Tool::input_schema` / `output_schema` as JSON Schema 2020-12 | structured output conforms when declared |
| Caching hints | `ttlMs >= 0`, `cacheScope ∈ {"public","private"}` | per caching.mdx; negative TTL treated as 0 |

## Behavior table (baseline)

| Row set | Modern (2026-07-28) | Compat (2025-11-25) |
| --- | --- | --- |
| Row count | 304 | 120 (+1 baseline CLAIM-03) |
| `resultType` | REQUIRED on results | absent → treated `complete` |
| `includeContext` | `none` default; `thisServer`/`allServers` deprecated (SEP-2596) | same fields, soft-deprecated |
| Cache hints | `ttlMs`/`cacheScope` REQUIRED where declared | absent tolerable |
| Schema authority | `schema/2026-07-28/*` | `schema/2025-11-25/*` |

## Compatibility and deprecation decisions

- **Soft-deprecated `includeContext` values** (`thisServer`,
  `allServers`): accepted for compatibility on both revisions but never
  emitted by GMCP; new code emits `none` or omits the field (SEP-2596).
- **`adapter` disposition (110 rows)**: these clauses describe the
  client's or host's side of a contract GMCP does not own (e.g. client
  sampling duties); GMCP's obligation is faithfully carrying the fields
  — representation, not behavior ownership.
- **`optional` disposition (78 rows)**: `may`/`should_not` leeway is
  preserved; the canonical model keeps the field so a peer's permitted
  use is never a parse failure.
- **`resultType` era difference**: pinned in VC-203-002/003 — emitted on
  the modern column, absent for 2025-11-25 peers.

## Threat model

- **Extension-field injection**: unknown keys could smuggle directives;
  policy is preserve-but-never-interpret — unknown fields round-trip but
  cannot alter dispatch or authorization.
- **Oversized/base64 payload abuse**: binary media and schema bodies are
  bounded by the VC-203-002 wire budgets; exceeding them earns the same
  typed `-32600`, never truncation.
- **URI/MIME/Unicode spoofing**: resource links, icons, and `data:` URIs
  are validated at the boundary; confusable or malformed values fail
  typed rather than being silently rewritten.
- **External reference (`$ref`) smuggling**: JSON Schema external
  references are resolved only against the pinned corpus — no network
  fetch, no peer-controlled schema injection.
- **Schema-poisoning via structured output**: a result claiming to
  conform to a declared `outputSchema` is validated; non-conformance is
  a typed failure, not a warning.

## SLOs and resource budgets

- Round-trip: every retained fixture `serialize → parse` yields the
  identical canonical value, including unknown extension fields.
- Validation cost is bounded: schema and payload sizes stay within the
  VC-203-002 `max_rpc_body_bytes`/`max_request_body_bytes` budgets; no
  unbounded recursion on nested schemas or content trees.
- Serialization is deterministic: canonical JSON output for equal
  values is byte-identical, so evidence hashes are stable.
- Compatibility overhead is zero-cost: legacy-only fields never allocate
  on the modern path.

## Fixtures

`tests/gmcp_requirements.rs` and the vector's later work packages replay
fixtures under `.agents/gmcp/fixtures/` (convention: `comment` +
`request`/`payload` + `expect`/`expect_code` + `era`). Seeds shipped
with this contract:

- `positive/content-block-annotated-2026-07-28.json` — a fully annotated
  `resource_link` content block with icon, audience, priority, and an
  unknown extension field; `expect: result`, `era: modern`.
- `negative/content-block-bad-mime.json` — an `image` content block
  whose `mimeType` is malformed; `expect: error`, `era: modern`.

## Evidence locations

`.agents/gmcp/evidence/VC-203-008.jsonl` is this vector's append-only
evidence artifact (convention: `.agents/gmcp/evidence/README.md`). Each
record carries `requirement`, `check`, `exit`, `detail`, `recorded_unix`,
`artifact_sha256`, and `identity`; artifact hashes sign the canonical
ledger bytes so a changed corpus invalidates stale proof.

## Ambiguities resolved

- **Three renderings, one schema**: `.json` is machine-canonical, `.ts`
  the type authority, `.mdx` the human rendering; the contract binds all
  three pinned files so drift in any rendering is detected.
- **`adapter` vs `modern` rows**: rows marked `adapter` describe a
  contract the peer owns; GMCP's duty is lossless carriage, not
  behavior implementation.
- **Unknown `type` tags in `ContentBlock`**: `non_exhaustive` serde —
  an unrecognized variant is a typed deserialization error at the
  boundary, never a silently dropped field.
- **Negative `ttlMs`**: clients treat it as `0` (immediately stale);
  servers MUST emit `>= 0` (caching.mdx lines 58–60).
- **Extension settings objects**: empty object means support with no
  settings; keys MUST follow the `io.modelcontextprotocol/*` namespacing
  rule for protocol-reserved fields.
