# Contract GMCP-203-016-01 — First-class extension framework and extension governance

Status: **contract pinned** (requirement GMCP-203-016-01, vector VC-203-016)
Baseline date: **2026-10-25**

## Normative clause

Namespaced MCP extensions are first-class: the protocol data model carries
them, discovery negotiates them, and governance versions, isolates and
disables them — without forking core protocol code and without weakening
the modern path.

The contract binds these obligations:

1. **Data model.** Every spec object that may carry extensions exposes an
   explicit `extensions` map keyed by the peer's own namespaced key; a
   parse → serialize round-trip preserves every unknown member verbatim,
   including keys the current revision does not name.
2. **Discovery and negotiation.** An extension is discoverable through the
   capability maps exchanged at initialization and per request; an unknown
   extension is retained as data, never rejected on sight, and never
   silently renamed.
3. **Versioning.** An extension's advertised version is part of the
   negotiation: a peer that requires a version the receiver does not serve
   fails explicitly with a typed error that names the requested and the
   supported versions.
4. **Isolation and disablement.** Extensions are isolated per connection and
   per extension id; disabling or failing one extension never disables the
   core surface or another extension, and no extension state crosses a
   tenant or connection boundary.
5. **Collision and downgrade.** Two extensions claiming the same key are a
   collision that fails closed rather than a last-writer-wins merge, and a
   negotiated downgrade is visible in the exchanged capabilities.
6. **Governance.** Official and experimental extension status is visible,
   deprecated extensions carry a removal policy and telemetry, and a watch
   process opens roadmap work for every newly published official extension.

Clauses 1–2 are satisfied by the shipped data model today
(`crates/susi-gmcp/src/content.rs`); clauses 3–6 are the normative
obligations the production task GMCP-203-016-02 must implement on the same
seam. A clause is honored only when its production path implements it and
the acceptance test proves it; prose alone is not delivery.

## Pinned sources

Extension authorities are pinned in `.agents/gmcp/sources.json` and
vendored under `.agents/gmcp/spec/`:

| repository | document | pin |
|---|---|---|
| modelcontextprotocol/ext-apps | `specification/2026-01-26/apps.mdx` | commit `82221c0c8ce7661efa6771c9d461511b1650495f` |
| modelcontextprotocol/ext-auth | `specification/stable/enterprise-managed-authorization.mdx` | commit `fb374c7db2b34f18ca9183882e0beecdf661892b` |
| modelcontextprotocol/ext-tasks | `specification/2026-07-28/tasks.md` | commit `899c1bbc869567670093face3591b39c28ce3219` |
| modelcontextprotocol/modelcontextprotocol | `docs/specification/2026-07-28/**` (extension and capability rules) | commit `86f6e0c675afea9a8f233e09b6faf566b126086c` |

## Canonical data model

The canonical name for an extension member set is the explicit map
`Extensions = Map<String, Value>` on each spec object, keyed by the
extension's own namespaced identifier (`io.modelcontextprotocol/<name>` or a
reverse-DNS vendor key). Extension data is explicit state on the object,
never ambient session state, and every object that can carry it exposes the
map rather than a catch-all `Value`.

The framework's canonical types — `ExtensionId`, `ExtensionVersion`,
`ExtensionStatus` (official, experimental, deprecated), and
`ExtensionRegistry` (per-connection negotiated set) — are declared here as
the contract for the production task; they do not exist yet and must not be
referenced by code until they do.

## Behavior table

Rows are normative. Each row names the level, the clause and the single
production path allowed to implement it.

| id | level | clause | production path |
|---|---|---|---|
| EXT-001 | must | An unknown or extension member on a spec object is preserved verbatim across parse → serialize. | crates/susi-gmcp/src/content.rs |
| EXT-002 | must | The extension map is keyed by the peer's own namespaced key and is never renamed or normalized. | crates/susi-gmcp/src/content.rs |
| EXT-003 | must | An extension field the current revision does not name is retained, not rejected or dropped. | crates/susi-gmcp/src/content.rs |
| EXT-004 | must | Extension clauses are classified in the ledger's `extension` category, never folded into another category. | crates/susi-gmcp/src/requirements_ledger.rs |
| EXT-005 | must | The server advertises its capabilities from an explicit builder that carries the extension maps. | crates/susi-gmcp/src/protocol.rs |
| EXT-006 | must | A peer's extension capability map is accepted as data; an unknown extension is not a protocol error. | crates/susi-gmcp/src/content.rs |
| EXT-007 | must | The wire gate forwards extension-bearing `_meta` byte-for-byte and never rewrites an extension key. | crates/susi-gmcp/src/jsonrpc_gate.rs |
| EXT-008 | should | A request that requires an unsupported extension version fails closed with a typed error naming the requested set. | crates/susi-gmcp/src/jsonrpc_gate.rs |
| EXT-009 | must | The canonical identity, capabilities and extension registry are explicit data, never ambient state. | crates/susi-gmcp/src/protocol.rs |
| EXT-010 | should | Positive and negative extension fixtures live with the acceptance test that proves them. | tests/gmcp_requirements.rs |
| EXT-011 | may | Official versus experimental extension status is visible through the pinned authorities. | .agents/gmcp/spec/ext-apps |

## Compatibility and deprecation decisions

Every clause carries an explicit disposition: modern=8, optional=3.
An extension is additive by construction: unknown keys round-trip, so a
modern peer and a legacy peer exchange extension data without either being
required to understand it. A deprecated extension keeps its key and payload
shape until its removal policy fires; removal is a new revision plus a
ledger row, never a silent drop. A downgrade is visible in the advertised
capabilities, and an extension that cannot be negotiated is disabled for
that connection only — the core surface stays available.

## Threat model

A malicious peer can forge an extension key it does not own, replay another
connection's extension state, or exploit an extension to escape isolation.
Keys are namespaced and attributed to their peer; extension state is
per-connection and never shared; collision is a typed refusal, not a merge;
an extension's failure is contained to that extension and that connection;
and no extension payload bypasses the content boundary or the size bound.

## SLOs and resource budgets

Extension negotiation completes within the session's declared deadline or
fails with a typed error. The extension registry is bounded per connection
(no unbounded key growth), extension payloads obey the same
`max_rpc_body_bytes` bound as every other message, and disabling an
extension releases its state within one request.

## Fixtures

The positive and negative fixtures for the pinned rows live with the
acceptance test `gmcp_req_203_016_01_contract` in
`tests/gmcp_requirements.rs`: the positive fixtures are the round-tripped
extension maps the data model preserves, and the negative fixtures are the
unknown-key, collision and unsupported-version cases the production task
must refuse.

## Evidence locations

The acceptance test binds this document to the behavior table and to the
pinned authorities; its retained output is the evidence, and a row without a
passing production-path test is not delivered.

## Ambiguities resolved

- **Vocabulary.** "Extension" means a namespaced member set carried on a
  spec object; it is never a synonym for a versioned core feature.
- **Where negotiation lives.** Capability exchange at initialization and the
  per-request `_meta` map are the only negotiation surfaces; a separate
  handshake is out of scope.
- **Isolation boundary.** Isolation is per connection and per extension id;
  a tenant boundary is inherited from the transport's authorization context,
  not invented by the framework.
- **Who owns status.** Official/experimental/deprecated status is owned by
  the pinned authorities and recorded in the ledger, not asserted by an
  implementation at runtime.

## Operations and non-claims

- **Rollout & rollback boundary**: `GmcpService::extensions()` isolates each `ExtensionRegistry` per service instance (`ExtensionRegistry::shipped()` preloads `io.modelcontextprotocol/tasks`), and extension packs under `~/.susi/extensions/<id>/` fall back to compiled defaults (`active = "default"`) whenever unloaded or absent. Reverting an upstream spec re-pin, extension pack, or capability adjustment restores previous extension semantics without altering user transcripts or requiring database migration.
- **Registry key growth note**: `ExtensionRegistry::capability_map()` surfaces only enabled registered entries while unknown peer extensions remain bounded by the transport framing and content-tree limits (`max_rpc_body_bytes`, `MAX_NODES = 10_000`, `MAX_DEPTH = 64`) at `jsonrpc_gate` / `CanonicalContentBlock::from_wire`.

## Operations and incident runbook

### Inspectable state

- `susi mcp ledger` prints the canonical JSON verification report (`rows`, `documents`, `revisions`, `dispositions`, `categories`, `vectors`, `verified`) matching the MCP `ledger_verify` tool verbatim and failing non-zero if any pinned corpus file drifts.
- `susi mcp status` emits inspectable MCP adapter control-plane status (`LeadingMcpManager::status`) in JSON.
- `susi mcp ledger --drift --refetched <dir>` executes deterministic, air-gapped standards drift detection across all pinned specification, schema, registry, and extension files against `.agents/gmcp/sources.json`.
- Extension pack lifecycle CLI & MCP tool parity (`susi extensions list|status|create|load|unload|validate` and MCP tools `extensions_list`, `extensions_status`, `extensions_validate`, `extensions_load`, `extensions_unload`): read-only `validate` audits manifest fields, `apiVersion` (`HOST_PACK_API_MAJOR = 1`), `KNOWN_PERMISSIONS`, SHA-256 digests, and Ed25519 signatures without mutating host `state.json`.
- Live runtime API inspection across served transports (`GmcpService::discover_result()`, `embedded_adapter`, `#[cfg(unix)]` socket adapter, `susi mcp serve` stdio, Streamable HTTP `/mcp`, `susi_wire_client::mcp::Session::open_with_capabilities` / `server_extensions()`, and `susi_core::mcp_client`):
  - `server/discover` and `initialize` advertise deterministic `capabilities.extensions` (with `io.modelcontextprotocol/tasks` enabled by default).
  - `ExtensionRegistry::status_report()` reports `[ { id, status, version, enabled } ]`, and `ExtensionRegistry::watch(OFFICIAL_EXTENSIONS)` emits `[GMCP] Adopt newly published official extension <id>` for every published official extension not yet registered.

### Configuration validation

- CLI flag validation fails closed when `--refetched <dir>` is supplied without `--drift` on `susi mcp ledger`.
- Host configuration validation (`susi_config::validate::validate_dir`) checks `~/.susi/config.json` before server startup and flags non-numeric `max_rpc_body_bytes` with `ConfigIssueKind::TypeMismatch`.
- `GmcpServer::run_stdio` rejects `max_rpc_body_bytes == 0` prior to reading frames, writing `[GMCP] Invalid max_rpc_body_bytes (0): must be > 0; refusing stdio transport` to `stderr` with zero bytes emitted on `stdout`; `http_service` logs `[GMCP] Invalid max_rpc_body_bytes (0); using bundled default for HTTP transport` to `stderr` and enforces HTTP payload ceilings (`413 Payload Too Large` past `max_rpc_body_bytes + 16`).
- Extension pack validation (`susi extensions validate` / `susi_config::extensions::validate_manifest`) fails closed on non-matching `apiVersion`, unknown permissions outside `KNOWN_PERMISSIONS` (`filesystem.read`, `filesystem.write`, `network.egress`, `process.exec`), path escapes (`../`) without `filesystem.read`, missing managed files, SHA-256 mismatches, or invalid Ed25519 signatures.

### Bounded metrics, logs and traces

- Content and transport bounds enforce memory, recursion, and payload isolation (`crates/susi-gmcp/src/{content.rs,stdio.rs,mcp/transport.rs,mcp/http.rs,server.rs}`):
  - `CanonicalContentBlock::from_wire` bounds: `MAX_NODES = 10_000`, `MAX_DEPTH = 64` (`ContentError::TooLarge`).
  - Transport frame/body bounds: stdio `DEFAULT_MAX_LINE_LENGTH = 16777216` (`16 MiB` via `BoundedLines`), HTTP `DEFAULT_MAX_REQUEST_BODY_BYTES = 4194304` (`4 MiB`), configured `max_rpc_body_bytes`, `MAX_TOOL_CALL_SECS = 1800`.
- W3C `traceparent` headers and `_meta["io.modelcontextprotocol/logLevel"]` operational telemetry remain bounded per request on HTTP `/mcp`, strip credentials/tokens from `notifications/message`, suppress `notifications/message` entirely when `logLevel` is omitted, and never bypass `_meta` schema or extension checks.
- Disabling an extension (`ExtensionRegistry::set_enabled(id, false)`) removes it from `capability_map()` within one request and isolates failures to that extension and connection.

### Diagnostics and incident triage

- `ExtensionError` typed diagnostics: `NotNamespaced` (key missing `vendor/name`), `Collision` (duplicate key registration refused closed instead of overwrite), `RequiredUnsupported` (peer requires an unregistered or disabled extension), `Downgrade` (required version mismatches served version).
- `ContentError` boundary diagnostics: `UnknownVariant`, `MissingField`, `BadUnicode`, `BadUri`, `BadMimeType`, `BadBase64`, `BadPriority`, `BadAudience`, `BadIcon`, `TooLarge`.
- Wire/HTTP JSON-RPC error codes: `-32600` (`Invalid Request` — malformed envelope, nesting bomb, oversized stdio `BoundedLines` frame, or modern `_meta` on a negotiated `2025-11-25` session), `-32602` (`Invalid params` — missing/invalid `_meta` protocol fields or legacy `params.protocolVersion`), `-32020` (`HEADER_MISMATCH`), `-32022` (`UNSUPPORTED_PROTOCOL_VERSION`), and HTTP `413` (oversized extension payload).
- `hash_drift` / `unfetchable`: named findings emitted by `susi mcp ledger --drift --refetched <dir>` whenever any pinned `VC-203-016` extension, schema, or registry artifact changes or is missing relative to `.agents/gmcp/sources.json`:
  - `ext-apps/specification/2026-01-26/apps.mdx` (`sha256: ee452a7d1b9b7fb900acfeb4d6932d3963375b0f3f37d196a4b93eb80312af0e`)
  - `ext-auth/specification/stable/enterprise-managed-authorization.mdx` (`sha256: df4fe01daec0eac6069e17a293d4ed7e197ab20463038d806f71e63b22fd8986`)
  - `ext-tasks/specification/2026-07-28/tasks.md` (`sha256: 5fc22b9b5ff248703927c69855f751a061c2d6170f4dc3923204452f859f91a5`)
  - `docs/specification/2026-07-28/index.mdx` (`sha256: c8151ab8d7f26ab46785fd34b23f7197db1a98ca8318967faa01e401909667b5`)
  - `schema/2026-07-28/schema.json` (`sha256: ef70b61f99b6d2e5e3b46863822eab08dff6a45bedc7a08914e0e5b133f40203`)
  - `schema/2026-07-28/schema.ts` (`sha256: 742750af0bb8c716e7030c4977c992b55d1adc4407e9e66997db5846baedc2cd`)
  - `schema/2025-11-25/schema.json` (`sha256: 268a5f82ba70fd7e4b6dc4aa1e64f116f74b4d0edcb69dc046829c79dd4e97e7`)
  - `schema/2025-11-25/schema.ts` (`sha256: e74b56e73b2e37bdb595f74ba22e428ad7f07aa3519355ba661d681298ed38ac`)
  - `registry/docs/reference/api/generic-registry-api.md` (`sha256: f5352a13d8b7309e28a23da998759f14c34945cb4008de10e51bba9bc47bb109`)
  - `registry/docs/reference/api/official-registry-api.md` (`sha256: 37989fc77551006d7fc3d34954c1f9d81a4edace8afc519d92b1c82a1b031dd9`)

### Examples

```text
# 1. Inspect GMCP ledger, MCP status, and extension pack state
susi mcp ledger
susi mcp status
susi extensions list
susi extensions status
susi extensions validate /path/to/pack

# 2. Run deterministic air-gapped drift check across pinned extension/schema specs
susi mcp ledger --drift --refetched .agents/gmcp/spec

# 3. Verify VC-203-016 operations gate and re-record append-only evidence
cargo test --offline --locked --test gmcp_requirements gmcp_req_203_016_06_operations
cargo local-release record-gmcp
cargo local-release record-gmcp --mastery
```

### Incident triage

Treat any `verified: false` drift finding (`hash_drift` or `unfetchable`), missing/invalid extension key collision guard (`Collision` / `NotNamespaced`), bypassed downgrade refusal (`Downgrade`), leaked unknown `_meta` or content extension keys, `stdout` non-JSON frame leak on zero `max_rpc_body_bytes`, or pack admission bypass as a release-blocking incident. Preserve the `susi mcp ledger --drift` JSON report, `susi extensions status`, offending manifest or JSON-RPC frame, server `stderr` diagnostics, and latest `.agents/gmcp/evidence/*.jsonl` proof records before remediation, then re-verify `gmcp_req_203_016`.

### Migration (extension governance and re-pinning upstream specs)

1. **Adopting newly published official extensions**: `ExtensionRegistry::watch(OFFICIAL_EXTENSIONS)` flags any official extension not yet registered in `ExtensionRegistry::shipped()`. Pin its upstream authority in `.agents/gmcp/sources.json`, vendor under `.agents/gmcp/spec/`, register explicit roadmap tasks, and advertise it via `ExtensionRegistry` rather than core protocol branches.
2. **Upstream re-pinning**: Update changed extension documents under `.agents/gmcp/spec/` and SHA-256 digests in `.agents/gmcp/sources.json` alongside `.agents/gmcp/requirements-ledger.json`, update `docs/gmcp/contract-203-016.md`, re-run `gmcp_req_203_016_01..06` and `check-gmcp-roadmap`, and record fresh two-phase `record-gmcp` proof records.

### Rollback

All extension specifications, contract prose (`docs/gmcp/contract-203-016.md`), `sources.json`, tests, and `.agents/gmcp/evidence/*.jsonl` chains are immutable, versioned Git artifacts; runtime protocol extension state is per-instance in memory, and extension pack activation cleanly reverts via `susi extensions unload <id>` (restoring compiled defaults with files intact). To roll back a specification re-pin or framework change, run `git revert <commit>`, re-validate `~/.susi/config.json` (`susi_config::validate::validate_dir`), verify `susi mcp ledger --drift --refetched .agents/gmcp/spec` passes (`verified: true`), re-run `gmcp_req_203_016_06_operations`, and append updated proof lines without deleting historical `.agents/gmcp/evidence/*.jsonl` entries.

### Production guarantees and non-claims

- Production guarantees cover `crates/susi-gmcp/src/{extensions.rs,content.rs,protocol.rs,jsonrpc_gate.rs,server.rs,stdio.rs,requirements_ledger.rs}`, `crates/susi-wire-client/src/mcp.rs`, and `crates/susi-config/src/extensions.rs` as proven across `gmcp_req_203_016_01..06` and retained evidence artifacts.
- Non-claim: Implicit acceptance of unknown optional extensions (`EXT-002`, `EXT-006`) applies only to unrequired peer keys; callers requiring hard support for an extension MUST pass explicit required IDs to `ExtensionRegistry::negotiate` so unsupported or disabled extensions fail closed with `ExtensionError::RequiredUnsupported`.
- Non-claim: Negotiated protocol extension state is per-session/per-instance only and is never persisted across reconnects; peers MUST re-exchange `capabilities.extensions` on every `initialize` or per-request `_meta` envelope.
