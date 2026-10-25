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
