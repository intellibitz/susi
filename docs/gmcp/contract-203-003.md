# GMCP contract — VC-203-003: stateless lifecycle and server discovery

Normative contract for the modern `2026-07-28` lifecycle: no
`initialize` ceremony or protocol-layer session identifier, per-request
version and client-capability validation, server identity on every
discovery result, a complete and cacheable `server/discover`, explicit
handles for all cross-call state, and no negotiated-state leakage
between concurrent requests from changing clients.

This document is the contract for the vector. It binds the mastery
target to pinned sources, names the canonical production path and data
model, fixes the behavior baseline per revision, records every
compatibility and deprecation decision, defines the threat model and
resource budgets, and locates the fixtures and evidence the work
packages must produce. `docs/gmcp/contract-203-001.md` governs the
ledger machinery itself; `docs/gmcp/contract-203-002.md` governs the
JSON-RPC substrate this lifecycle rides on; this contract governs what
VC-203-003 must prove.

## Normative clause

> For MCP 2026-07-28 and later GMCP is genuinely stateless at the
> protocol layer: no initialize ceremony or session identifier is
> required, every request carries and validates version plus client
> capabilities, every result identifies the server, server/discover is
> complete and cacheable, and explicit handles bind all cross-call
> state. Concurrent requests from changing clients cannot leak
> negotiated state. Done means production-path tests and retained
> evidence prove this work package; task closure or roadmap wording
> alone is not evidence.

Binding: the rows of `.agents/gmcp/requirements-ledger.json` whose
`trace.vector == "VC-203-003"` are the complete normative definition.
The ledger carries **6 rows** for this vector — **1** repository
`CLAIM` row pinning the roadmap outcome and **5** `behavior` rows from
revision `2026-07-28` (`server/discover.mdx`): the `MUST` implement
obligation (line 8), the stdio-era probe `SHOULD` (line 78), the
`serverInfo` `SHOULD` (line 99), and the two self-reported-identity
`SHOULD NOT` clauses (lines 105–106). Verbatim clauses are enforced
byte-for-byte by the production verifier
(`susi_gmcp::requirements_ledger::verify_pinned_ledger`) and by
`scripts/check-gmcp-ledger.py`; a row that no longer quotes its pinned
source fails closed.

## Pinned sources

All rows resolve to the VC-203-001 pinned corpus
(`.agents/gmcp/spec/`, hashed in `.agents/gmcp/sources.json`):

| Document | Rows | Revision |
|---|---|---|
| `docs/specification/2026-07-28/server/discover.mdx` | 5 | modern |
| `docs/gmcp-roadmap.md` (`CLAIM-01`, repository baseline) | 1 | — |

Adjacent normative material this contract interprets (pinned in the
same corpus, traced by other vectors but behaviorally bound here):

| Document | Governs |
|---|---|
| `docs/specification/2026-07-28/basic/versioning.mdx` | protocol-version negotiation, `supported`/`requested` error data, backward-compatibility matrix |
| `docs/specification/2026-07-28/server/utilities/caching.mdx` | `ttlMs` / `cacheScope` semantics on cacheable results |
| `docs/specification/2026-07-28/basic/transports/stdio.mdx` | discover-first probe and legacy fallback rules on stdio |
| `docs/specification/2026-07-28/basic/index.mdx` | reserved `_meta` keys and naming rules |
| `docs/specification/2025-11-25/basic/lifecycle.mdx` | the legacy `initialize` lifecycle this vector replaces on the modern era |

Upstream drift on any pinned file fails closed via `susi mcp ledger
--drift` (live) or `--refetched <dir>` (deterministic) — the same check
VC-203-001 shipped.

## Canonical data model

The discovery operation and per-request protocol context are owned by
susi-gmcp on top of the vendored server crate. One implementation
serves every entry point (stdio, HTTP, direct dispatch).

| Concept | Canonical owner |
|---|---|
| `server/discover` request handling | `susi_gmcp::protocol::GmcpService` (served before any handshake on the modern path) |
| `DiscoverResult` wire shape | result object: `resultType: "complete"`, `supportedVersions` (non-empty), `capabilities`, `_meta["io.modelcontextprotocol/serverInfo"]`, optional `instructions`, `ttlMs`, `cacheScope` |
| Per-request protocol context | `params._meta` reserved keys — `io.modelcontextprotocol/protocolVersion` + `io.modelcontextprotocol/clientCapabilities` REQUIRED on every modern request |
| Era classification | `susi_gmcp::jsonrpc_gate::{EraState, classify_line}` — per-request `_meta` pins the modern era; `initialize` pins the negotiated revision |
| Server identity | `_meta["io.modelcontextprotocol/serverInfo"]` = `{name, version}` — `{"name": "susi-gmcp", "version": <crate version>}` |
| Explicit cross-call handles | `tasks` extension handles (`capabilities.extensions["io.modelcontextprotocol/tasks"]`), `_meta.progressToken`, pagination `cursor`, and `inputResponses`/`requestState` resumption payloads — the closed set |
| Supported revision registry | `jsonrpc_gate::MODERN_SUPPORTED_VERSIONS` + the negotiated-legacy set (`2024-11-05` through `2025-11-25`) |

## Behavior table (baseline)

| Area | modern `2026-07-28` | compatibility `2025-11-25` and earlier |
|---|---|---|
| `initialize` ceremony | not required — absent entirely | required before non-`discover`/`ping` requests |
| Protocol session identifier | none — requests are self-contained | none at the protocol layer (HTTP transport session routing is bookkeeping only, never consulted for semantics) |
| Version validation | per request: `_meta.protocolVersion` REQUIRED; unknown → `-32022` with `data.supported`/`data.requested` | negotiated once at `initialize` |
| Capability validation | per request: `_meta.clientCapabilities` REQUIRED; missing → `-32602` | exchanged once at `initialize` |
| `server/discover` | MUST implement; answerable without any handshake; result complete + cacheable | MUST implement — it is the probe a dual-era client sends first |
| Server identity in result | `_meta.serverInfo` on every `server/discover` result (spec `SHOULD` → pinned REQUIRED for GMCP) | `serverInfo` in the `initialize` result |
| `DiscoverResult` fields | `supportedVersions` non-empty; `capabilities`; `ttlMs` ≥ 0 REQUIRED; `cacheScope` ∈ {`public`,`private`} REQUIRED; `instructions` optional | same wire shape |
| Cross-call state | explicit handles only: task handles, `progressToken`, `cursor`, `requestState`/`inputResponses` | same — legacy adds negotiated-session context via `initialize` result |
| Concurrent changing clients | each request independently versioned; no negotiated state exists to leak | negotiated context is per-connection and never crosses requests |

## Compatibility and deprecation decisions

- **Legacy `initialize` lifecycle retained.** The `2025-11-25` and
  earlier path negotiates once at `initialize`; VC-203-002's gate
  era-tracks per connection. This vector changes nothing on that path.
- **`server/discover` precedes the handshake.** Discover and `ping`
  are the only requests answered before `initialize` on a
  legacy-intended connection — the spec's stdio backward-compatibility
  probe requires it. Any other pre-`initialize` request fails the
  handshake deterministically.
- **`serverInfo` elevated to REQUIRED** on GMCP's own discover result:
  the spec rates it `SHOULD`, but the vector's "every result identifies
  the server" resolves to a non-optional `serverInfo` on the discover
  payload. It MAY appear on other results; it is not required there.
- **Cache baseline pinned to the shipped values** `ttlMs: 0`,
  `cacheScope: "private"`: the result varies with server version and
  build configuration, so the conservative hint is immediate staleness
  and no shared caching. A later change to a positive `ttlMs` must
  keep `ttlMs >= 0` and revisit `cacheScope` deliberately.
- **`instructions` omitted** until a real instruction set exists —
  fabricating guidance is worse than the optional field's absence.
- **Discover is version-checked like every other request.** A
  `server/discover` carrying an unsupported `_meta.protocolVersion`
  earns `-32022` — the spec's "invoke any RPC inline and handle
  `UnsupportedProtocolVersionError`" flow applies to discover itself;
  its `data.supported` list is the fallback the client needs anyway.
- **Transport session ids are not protocol state.** Any
  transport-scoped session/routing identifier (HTTP resumability) MAY
  exist but MUST NOT carry negotiated semantics: a request without its
  own `_meta` protocol fields on the modern path is malformed
  regardless of which connection delivered it.

## Threat model

| Threat | Control |
|---|---|
| `serverInfo` spoofing / trust escalation | `serverInfo` is self-reported and unverified: display, logging, and debugging only; consumers MUST NOT change behavior or make security decisions from it (discover.mdx lines 105–106) |
| Version confusion | per-request `_meta.protocolVersion` validated on every modern request; unknown → deterministic `-32022` with `data.supported` |
| Negotiated-state leak across concurrent requests | no negotiated state exists on the modern path — version and capabilities are re-declared per request, so a client whose capabilities change mid-stream cannot contaminate another client's request |
| Era-ambiguous dispatch | per-request classification: a modern-metadata request on a negotiated-legacy session earns deterministic `-32600` (never a silent drop, never a silent era flip); `initialize`/`server/discover` stay dual-era |
| Discover as fingerprinting surface | the result exposes identity and capabilities only — no session data, no per-client state; identical for every caller |
| Cache poisoning / stale discovery | `ttlMs >= 0` enforced; `cacheScope` respected; results carrying `inputResponses`/`requestState` MUST NOT be cached (caching.mdx) |
| Hidden cross-call state | the explicit-handle set is closed: task handles, `progressToken`, `cursor`, `requestState`/`inputResponses`; any other retained client context is a defect |

## SLOs and resource budgets

- `server/discover` is static per process build: its result MUST
  contain `resultType: "complete"`, non-empty `supportedVersions`,
  `capabilities`, `_meta.serverInfo` (`{name, version}`), `ttlMs >= 0`,
  and `cacheScope` — every call, every transport.
- A modern request allocates no persistent per-client state: request
  handling is bounded by the message-size budgets VC-203-002 pins
  (`max_rpc_body_bytes` / `max_request_body_bytes`), and no
  session-scoped allocation survives the response.
- Discover latency is bounded by serialization of a constant payload;
  no I/O beyond the wire write is permitted on the serving path.
- Unsupported-version handling on discover is one `-32022` error —
  deterministic, no retries, no partial result.

## Fixtures

`tests/gmcp_requirements.rs` and the vector's later work packages replay
the fixtures under `.agents/gmcp/fixtures/` (convention: `comment` +
`request` + `expect`/`expect_code` + `era`). Seeds shipped with this
contract:

- `positive/server-discover-fullmeta-2026-07-28.json` — `server/discover`
  carrying the complete modern `_meta` (protocolVersion + clientInfo +
  clientCapabilities), the discover.mdx example shape; `expect:
  result`, `era: modern`.
- `negative/server-discover-unsupported-version.json` — `server/discover`
  with `_meta.protocolVersion: "1999-01-01"`; `expect: error`,
  `expect_code: -32022`, `era: modern`.

Existing fixtures this vector also binds:

- `positive/server-discover-2026-07-28-meta.json` — the corrected
  minimal-`_meta` discover form (VC-203-002 seed).
- `positive/request-2026-07-28-meta.json` — a modern non-discover
  request answered without any handshake (the statelessness witness).
- `negative/request-meta-missing-2026-07-28.json` — modern request
  missing required `_meta`; `expect_code: -32602`.
- `positive/server-discover-2026-07-28.json` — retained verbatim
  divergence record (`params.protocolVersion` form; `divergences.json`).

## Evidence locations

`.agents/gmcp/evidence/VC-203-003.jsonl` is this vector's append-only
evidence artifact (convention: `.agents/gmcp/evidence/README.md`). Each
work package records its requirement id, check command, exit status,
detail, timestamp, artifact hash, and identity.

## Ambiguities resolved

- **"Every result identifies the server"**: the spec carries
  `serverInfo` only on the discover result as a `SHOULD`. Resolution:
  REQUIRED on `server/discover` results; permitted but not required
  elsewhere. Results on non-discover methods are identified through the
  discover contract, not per-result identity fields.
- **"Explicit handles"**: enumerated as a closed set — task handles
  (the `io.modelcontextprotocol/tasks` extension), `progressToken`,
  pagination `cursor`, and `requestState`/`inputResponses` resumption
  payloads. No other cross-call binding is permitted.
- **"No session identifier"**: scoped to the protocol layer.
  Transport-level session/routing identifiers may exist for connection
  bookkeeping but carry zero negotiated semantics.
- **Era-ambiguous first request**: the spec permits
  implementation-defined behavior; GMCP's resolution is deterministic —
  `server/discover` and `ping` answer on both eras before any
  handshake; everything else requires either `_meta` (modern) or a
  completed `initialize` (legacy) and fails the handshake otherwise.
- **Era-ambiguous request on a negotiated session**: a request
  carrying `io.modelcontextprotocol/*` metadata on a
  negotiated-legacy session would be silently dropped by the service
  layer — GMCP answers it `-32600` instead, and the rejection never
  flips the negotiated era, so a hostile or confused request cannot
  poison later legacy requests on the same connection. The dual-era
  `server/discover`/`initialize` methods are exempt.
- **Discover under an unsupported requested version**: resolved to
  `-32022` rather than an unconditional answer — the discover result's
  `supportedVersions` is itself the negotiation payload, so version
  checking discover is consistent and gives the client the same
  fallback data.
- **`serverInfo` trust level**: self-reported metadata; never
  consulted by the server for its own decisions and never a basis for
  client behavior or security decisions (verbatim discover.mdx
  105–106).
