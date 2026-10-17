# GMCP contract — VC-203-002: JSON-RPC substrate

Normative requirements ledger and standards drift for the JSON-RPC
substrate: identifiers, notifications, cancellation, progress tokens,
typed errors, required `resultType`, per-request protocol/capability
metadata, trace context, and deterministic unsupported-version behavior
for every supported revision.

This document is the contract for the vector. It binds the mastery
target to pinned sources, names the canonical production path and data
model, fixes the behavior baseline per revision, records every
compatibility and deprecation decision, defines the threat model and
resource budgets, and locates the fixtures and evidence the work
packages must produce. `docs/gmcp/contract-203-001.md` governs the
ledger machinery itself; this contract governs what VC-203-002 must
prove.

## Normative clause

> GMCP implements the complete JSON-RPC substrate for every supported
> revision: identifiers and batches where applicable, notifications,
> cancellation, progress tokens, typed standard and MCP errors, required
> `resultType`, per-request protocol/capability metadata, trace context,
> and deterministic unsupported-version behavior. Malformed, duplicated,
> reordered, oversized, and adversarial messages fail predictably
> without corrupting another request.

Binding: the rows of `.agents/gmcp/requirements-ledger.json` whose
`trace.vector == "VC-203-002"` are the complete normative definition.
The ledger carries **157 rows** for this vector — **107** from revision
`2026-07-28` (modern) and **50** from `2025-11-25` (compatibility):
149 `behavior`, 6 `error`, 2 `security`. Verbatim clauses are enforced
byte-for-byte by the production verifier
(`susi_gmcp::requirements_ledger::verify_pinned_ledger`) and by
`scripts/check-gmcp-ledger.py`; a row that no longer quotes its pinned
source fails closed.

## Pinned sources

All rows resolve to the VC-203-001 pinned corpus
(`.agents/gmcp/spec/`, hashed in `.agents/gmcp/sources.json`):

| Document | Rows | Revision |
|---|---|---|
| `docs/specification/2026-07-28/basic/index.mdx` | 72 | modern |
| `docs/specification/2025-11-25/basic/index.mdx` | 37 | compatibility |
| `docs/specification/2026-07-28/basic/patterns/cancellation.mdx` | 19 | modern |
| `docs/specification/2026-07-28/basic/versioning.mdx` | 13 | modern |
| `docs/specification/2025-11-25/basic/utilities/cancellation.mdx` | 10 | compatibility |
| `docs/specification/2026-07-28/index.mdx` | 3 | modern |
| `docs/specification/2025-11-25/index.mdx` | 3 | compatibility |

Upstream drift on any pinned file fails closed via `susi mcp ledger
--drift` (live) or `--refetched <dir>` (deterministic) — the same check
VC-203-001 shipped.

## Canonical data model

The JSON-RPC envelope and dispatch are owned by the vendored server
crate; GMCP policy lives in susi-gmcp. One implementation serves every
entry point.

| Concept | Canonical owner |
|---|---|
| Wire types (`jsonrpc`, `id`, `method`, `params`, `result`, `error`) | `susi_vendor_mcp_server` (rmcp) `JsonRpcMessage` / `Request` / `Notification` / `Response` / `ErrorData` |
| Server role (dispatch, negotiation, tool routing) | `susi_gmcp::protocol::GmcpService` (`serve`, `session`) |
| Stdio transport bound | `susi_gmcp::stdio::BoundedLines` — one JSON value per line, line bytes ≤ `max_rpc_body_bytes` |
| Streamable HTTP transport | `susi_gmcp::server::http_service` — `max_request_body_bytes`, allowed-hosts, Origin validation |
| Client role | `susi_core::mcp_client` (`call_tool`, `session_call`, session-aware GMCP-HTTP) |
| Direct dispatch (no wire) | `susi_gmcp::direct_call::call_tool_direct` |
| Registry dispatch (daemon/plane-bus) | `susi_tools::ToolRegistry` via `susi_daemon::gmcp_bootstrap::bootstrap_registry` |

`_meta` is an open map; the reserved keys this contract binds are
`progressToken`, `io.modelcontextprotocol/protocolVersion`,
`io.modelcontextprotocol/clientInfo`,
`io.modelcontextprotocol/clientCapabilities`,
`io.modelcontextprotocol/logLevel`,
`io.modelcontextprotocol/subscriptionId`, and the OpenTelemetry keys
`traceparent` / `tracestate` / `baggage` (W3C Trace Context format).

## Behavior table (baseline)

| Area | modern `2026-07-28` | compatibility `2025-11-25` |
|---|---|---|
| Request id | string \| integer; **MUST NOT** be `null` | same |
| Notifications | no `id`; **no** response | same |
| Batches | undefined → malformed (see ambiguities) | same |
| Protocol metadata | required per-request `_meta`: `protocolVersion` + `clientCapabilities`; missing → `-32602` (HTTP 400) | negotiated once at `initialize`; `MCP-Protocol-Version` header on HTTP after |
| Version unknown/unsupported | `UnsupportedProtocolVersionError` `-32022` with `data.supported`/`data.requested`; `server/discover` MUST be served | `initialize` replies with a supported version; client SHOULD disconnect if unsupported |
| Cancellation | `notifications/cancelled` (requestId, optional reason); `initialize` MUST NOT be cancelled; task-augmented → `tasks/cancel` | same notification; `initialize` MUST NOT be cancelled |
| Progress | `_meta.progressToken` opts into `notifications/progress` | same |
| `resultType` | required on results; `complete` \| `input_required`; unrecognized → invalid | absent → treated as `complete` |
| Errors | JSON-RPC `-32700`/`-32600..-32603`; MCP `-32020..-32099`; legacy `-32002` accepted from older servers | `-32042` URL elicitation (this revision only); `-32002` resource-not-found |
| Trace context | `traceparent`/`tracestate`/`baggage` in `_meta`, W3C format | not defined — pass through untouched |
| `_meta` naming | mandatory prefix; alnum ends; `-_.` inside | same |

## Compatibility and deprecation decisions

- **Legacy error sub-range `-32000..-32019`**: only `-32002` is
  accepted from servers; GMCP never emits the rest of the sub-range.
  `-32002` means resource-not-found on `2025-11-25` and earlier; on the
  modern path `-32602` is emitted and `-32002` is still tolerated
  inbound from older servers.
- **`-32042`** (URL elicitation required) exists only on `2025-11-25`;
  emitted only when serving that revision.
- **Absent `resultType`**: treated as `"complete"` for backward
  compatibility with pre-`2026-07-28` servers.
- **Era-ambiguous requests**: a client SHOULD call `server/discover`
  first so a modern-only server fails deterministically rather than
  silently applying legacy semantics (versioning.mdx matrix).
- **`params.protocolVersion` is not a substitute for `_meta`**: the
  modern path requires the `_meta` fields; the retained
  `positive/server-discover-2026-07-28.json` fixture documents the
  `params`-only form and `divergences.json` records its observed
  `-32602` rejection.

## Threat model

| Threat | Control |
|---|---|
| Malformed/oversized message exhausting memory | `BoundedLines` + `max_rpc_body_bytes` on stdio; `max_request_body_bytes` on HTTP |
| Batch array smuggling (undefined in MCP) | rejected as malformed — see ambiguities |
| `id: null` treated as notification | MUST be rejected per `MCP-2026-0728-basic-index-0047`; retained divergence: the current stdio path drops it silently (`divergences.json`) |
| Cross-request corruption | every message parses and dispatches independently; a poisoned request must not corrupt its neighbors |
| Version confusion / era-ambiguous dispatch | per-request `_meta` fields required on modern; `server/discover` + `-32022` fail deterministically |
| Trace-context spoofing | `traceparent` is propagation, not authorization; values are validated as W3C format and never trusted |
| Cancellation abuse | `initialize` MUST NOT be cancelled; task requests cancel only via `tasks/cancel` |

## SLOs and resource budgets

- One JSON value per stdio line; a line over `max_rpc_body_bytes` fails
  bounded, never reads unbounded.
- Malformed input yields a typed JSON-RPC error (or a documented
  divergence) — never a panic, hang, or corrupted sibling request.
- Unsupported-version handling is deterministic: one `-32022` error
  carrying `data.supported`, no retries, no partial dispatch.
- Dispatch latency is outside this vector's budget; the bounds it owns
  are message-size and per-request isolation.

## Fixtures

`tests/gmcp_requirements.rs` and the vector's later work packages replay
the fixtures under `.agents/gmcp/fixtures/`, each a single JSON object
with `comment` (the ledger row or decision exercised), `request` (the
raw payload — a JSON object, or a non-object for malformed-wire cases),
`expect` (`result`, `error`, or `none` for notifications), and for
negative fixtures `expect_code` (the JSON-RPC error code a conformant
endpoint returns). Seeds shipped with this contract:

- `positive/request-2026-07-28-meta.json` — `tools/list` carrying the
  required `_meta` protocol fields.
- `positive/request-progress-token.json` — request with
  `_meta.progressToken` opting into progress.
- `positive/request-trace-context.json` — request with W3C
  `traceparent`/`tracestate` in `_meta`.
- `positive/notification-cancelled.json` — `notifications/cancelled`;
  `expect: none`.
- `negative/request-batch-array.json` — a JSON-RPC batch array;
  `expect: error` (malformed — see ambiguities).
- `negative/request-meta-missing-2026-07-28.json` — modern request
  without the required `_meta` fields; `expect_code: -32602`.
- `negative/request-unsupported-version.json` — `_meta` version
  `1900-01-01`; `expect_code: -32022`.

`negative/request-null-id.json` (VC-203-001 seed) also binds to this
vector's `MCP-2026-0728-basic-index-0047` row.

## Evidence locations

`.agents/gmcp/evidence/VC-203-002.jsonl` is this vector's append-only
evidence artifact (convention: `.agents/gmcp/evidence/README.md`). Each
work package records its requirement id, check command, exit status,
detail, timestamp, artifact hash, and identity.

## Ambiguities resolved

- **Batches**: neither pinned revision defines JSON-RPC batch
  messages. A JSON array on the wire is malformed input and fails
  closed (`-32700`/`-32600` class) — "batches where applicable"
  resolves to *not applicable*.
- **`id: null`**: spec-unambiguous (MUST NOT), but the current stdio
  path drops it without a response rather than replying `-32600`;
  retained in `divergences.json` — conformance requires a typed error,
  the divergence is the repair record.
- **`_meta` requiredness**: the required per-request protocol fields
  apply to the modern path only; the compatibility path negotiates at
  `initialize` and does not require `_meta` on subsequent requests.
- **Notifications**: never produce a response, including malformed
  notification shapes — a malformed notification is dropped, a
  malformed request errors.
- **Duplicated/reordered messages**: each line is an independent
  message; ordering is not a protocol invariant on stdio, and a
  duplicated request id produces a second independent dispatch (id
  reuse across concurrent in-flight requests is a client error the
  server is not required to police).
- **`expect: none`**: added to the fixture convention for
  notification-shaped inputs (no response permitted).

## Operations and incident runbook

### Inspectable state

- `susi mcp ledger` is the production health check. It emits one bounded
  JSON report containing `rows`, `documents`, `revisions`, `dispositions`,
  `categories`, `vectors`, and `verified`; a false verification exits
  non-zero before protocol traffic is trusted.
- `susi mcp call susi ledger_verify {}` exposes the same report through the
  tool surface. Comparing it with the CLI report is a transport-independent
  smoke check for direct, daemon, gateway, and stdio dispatch.
- `susi mcp ledger --drift --refetched <dir>` is the deterministic, air-gapped
  check. The directory is keyed by `sources.json` document paths and is bounded
  by the same source and artifact limits as the live check.

### Diagnostics and incident triage

Every drift finding has a stable `kind`: `hash_drift` means bytes differ from
the pinned source, while `unfetchable` means a required document is absent or
could not be retrieved. A non-zero command and `verified: false` are required
for either condition; callers must not continue with a partially checked
corpus. For a local failure, restore the named vendored file from git. For an
upstream `hash_drift`, preserve the report and begin the migration below. For
an `unfetchable` source, retry within the published bounds, then recover the
last known pin from git history rather than silently skipping it. The
`traceparent`, `tracestate`, and `baggage` fields are propagation diagnostics,
not authorization signals, and must never be used to bypass these checks.

### Incident triage

Treat a non-zero report as an incident until the named source or pin is
restored and the deterministic check is green again. Preserve the JSON report
and the corresponding evidence record with the incident for independent
reproduction.

### Migration (re-pinning the JSON-RPC standard)

1. Claim a task and record the upstream revision and reason for the re-pin.
2. Refresh `.agents/gmcp/spec/` and `sources.json` from that exact revision.
3. Run `scripts/check-gmcp-ledger.py --write`, review every changed
   JSON-RPC clause, and update fixtures, compatibility decisions, and the
   retained evidence record.
4. Run `susi mcp ledger`, the deterministic `--refetched` check, and the full
   GMCP acceptance gate before publishing the change.

### Rollback

The vendored specification, manifest, ledger, fixtures, and evidence are
committed artifacts. Roll back a bad re-pin with `git revert` of its commit,
then rerun the CLI and deterministic drift checks. This restores the prior
JSON-RPC semantics without a data migration; do not delete evidence rows or
rewrite a prior proof event.
