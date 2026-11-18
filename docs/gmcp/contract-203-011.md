# VC-203-011 contract

This contract binds every normative clause of vector VC-203-011. Each row in the behavior table is normative; its identifier, level, clause and production path are pinned.

## Normative clause

The MVP must implement the 39 pinned clauses below. Every row is normative, bound to its production path, and resolved before implementation.

## Pinned sources

| repository | document | sha256 | lines |
|---|---|---|---|
| modelcontextprotocol | docs/specification/2025-11-25/server/prompts.mdx | 5c4a5adefade5d9ecaa1446278a46d48c11fbfb4217bcd81c8fc004928a4abe1 | 30-285 |
| modelcontextprotocol | docs/specification/2025-11-25/server/utilities/completion.mdx | ea42d006ba3b3ab8deceb5ae22be17f79e40b26a207947459560018f91eb42e9 | 26-197 |
| modelcontextprotocol | docs/specification/2026-07-28/server/prompts.mdx | 85e550635bbbbaa90dff8738f98808c62987e8cfce1d75305b55ca43f3c546de | 16-335 |
| modelcontextprotocol | docs/specification/2026-07-28/server/utilities/completion.mdx | 1965c563aadb99e141131444caa81ff1860ed82bab314b286c40a0f0ba839f99 | 16-209 |

## Canonical data model

Each row names a canonical behavior; its ledger id is the canonical name and its production path is the only implementation site. Requests, responses and handles are explicit data, never ambient state.

## Behavior table

| id | level | clause | production path |
|---|---|---|---|
| MCP-2025-1125-server-prompts-mdx-0030 | must | Servers that support prompts **MUST** declare the `prompts` capability during | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-server-prompts-mdx-0144 | should | capability **SHOULD** send a notification: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-server-prompts-mdx-0226 | must | The image data **MUST** be base64-encoded and include a valid MIME type. This enables | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-server-prompts-mdx-0241 | must | The audio data MUST be base64-encoded and include a valid MIME type. This enables | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-server-prompts-mdx-0259 | must | Resources can contain either text or binary (blob) data and **MUST** include: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-server-prompts-mdx-0271 | should | Servers **SHOULD** return standard JSON-RPC errors for common failure cases: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-server-prompts-mdx-0279 | should | 1. Servers **SHOULD** validate prompt arguments before processing | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-server-prompts-mdx-0280 | should | 2. Clients **SHOULD** handle pagination for large prompt lists | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-server-prompts-mdx-0281 | should | 3. Both parties **SHOULD** respect capability negotiation | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-server-prompts-mdx-0285 | must | Implementations **MUST** carefully validate all prompt inputs and outputs to prevent | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-server-utilities-completion-mdx-0026 | must | Servers that support completions **MUST** declare the `completions` capability: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-server-utilities-completion-mdx-0175 | should | Servers **SHOULD** return standard JSON-RPC errors for common failure cases: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-server-utilities-completion-mdx-0184 | should | 1. Servers **SHOULD**: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-server-utilities-completion-mdx-0190 | should | 2. Clients **SHOULD**: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-server-utilities-completion-mdx-0197 | must | Implementations **MUST**: | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-prompts-mdx-0016 | must | `io.modelcontextprotocol/clientCapabilities`). Every request **MUST** include | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-prompts-mdx-0041 | must | Servers that support prompts **MUST** declare the `prompts` capability in their | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-prompts-mdx-0057 | must | Servers that declare the `prompts` capability **MUST** respond to `prompts/list` requests | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-prompts-mdx-0058 | may | with the set of prompts currently available to the requesting client. This set **MAY** be | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-prompts-mdx-0059 | may | empty and **MAY** change over time (see | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-prompts-mdx-0060 | must_not | [List Changed Notification](#list-changed-notification)), but **MUST NOT** vary | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-prompts-mdx-0062 | may | **MAY** vary by the authorization presented on the request — for example, returning | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-prompts-mdx-0165 | may | Servers **MAY** also respond to `prompts/get` with an [`InputRequiredResult`](/specification/2026-07-28/basic/patterns/mrtr#inputrequiredresult) to indicate that additional input is needed before the prompt can be resolved. This follows the | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-prompts-mdx-0170 | should | capability **SHOULD** send a notification to clients that have opened a | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-prompts-mdx-0256 | must | The image data **MUST** be base64-encoded and include a valid MIME type. This enables | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-prompts-mdx-0271 | must | The audio data MUST be base64-encoded and include a valid MIME type. This enables | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-prompts-mdx-0276 | may | Prompt messages **MAY** include links to | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-prompts-mdx-0309 | must | Resources can contain either text or binary (blob) data and **MUST** include: | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-prompts-mdx-0321 | should | Servers **SHOULD** return standard JSON-RPC errors for common failure cases: | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-prompts-mdx-0329 | should | 1. Servers **SHOULD** validate prompt arguments before processing | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-prompts-mdx-0330 | should | 2. Clients **SHOULD** handle pagination for large prompt lists | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-prompts-mdx-0331 | should | 3. Both parties **SHOULD** respect capability negotiation | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-prompts-mdx-0335 | must | Implementations **MUST** carefully validate all prompt inputs and outputs to prevent | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-utilities-completion-mdx-0016 | must | `io.modelcontextprotocol/clientCapabilities`). Every request **MUST** include | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-utilities-completion-mdx-0035 | must | Servers that support completions **MUST** declare the `completions` capability: | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-utilities-completion-mdx-0187 | should | Servers **SHOULD** return standard JSON-RPC errors for common failure cases: | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-utilities-completion-mdx-0196 | should | 1. Servers **SHOULD**: | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-utilities-completion-mdx-0202 | should | 2. Clients **SHOULD**: | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-utilities-completion-mdx-0209 | must | Implementations **MUST**: | crates/susi-gmcp/src/protocol.rs |

## Compatibility and deprecation decisions

Every clause carries an explicit disposition: adapter=15, modern=19, optional=5. A clause disposed as adapter is honored only through the explicit version adapter with downgrade telemetry and a removal policy.

## Threat model

A malicious peer can replay a handle, answer another principal's request, or stall past a deadline. Handles are principal-scoped and single-use; responses are validated against the request that created them; deadlines and cancellation are enforced before any side effect.

## SLOs and resource budgets

Every interaction completes within its declared deadline or is cancelled with a typed error. Bounded queues and per-principal handle budgets prevent unbounded growth.

## Fixtures

Positive and negative fixtures live in `.agents/gmcp/fixtures/`; current run records live in .susi/records/gmcp/evidence/VC-203-011.jsonl, keyed by row id. The tests consume both.

## Evidence locations

.susi/records/gmcp/evidence/VC-203-011.jsonl holds the retained evidence; the acceptance test binds this document to the ledger.

## Ambiguities resolved

All clauses are resolved by this table: the ledger id is the canonical name, the production path is the only allowed implementation site, and the disposition decides whether the clause is honored on the modern path or only through the version adapter.

## Operations and incident runbook

### Inspectable state

- `susi mcp ledger` is the production ledger verification CLI command. It emits a bounded JSON report exposing `rows`, `documents`, `revisions`, `dispositions`, `categories`, `vectors`, and `verified`; a `false` verdict exits non-zero before server metadata is trusted.
- `susi mcp status` emits the inspectable MCP server control-plane state (`LeadingMcpManager::status`).
- `susi mcp ledger --drift --refetched <dir>` executes deterministic, air-gapped standards drift verification against the four pinned prompt and completion specs (`docs/specification/2025-11-25/server/prompts.mdx`, `docs/specification/2025-11-25/server/utilities/completion.mdx`, `docs/specification/2026-07-28/server/prompts.mdx`, and `docs/specification/2026-07-28/server/utilities/completion.mdx`).
- Every served transport (`susi mcp serve` stdio, in-process duplex `embedded_adapter`, Streamable HTTP `/mcp`, and `#[cfg(unix)]` Unix-domain socket adapter) exposes identical live prompt/completion discovery and invocation state: `server/discover` / `initialize` advertising `capabilities.prompts.listChanged = true` and `capabilities.completions = {}`, `prompts/list`, `prompts/get`, `completion/complete`, and `tools/call ledger_verify` matching the CLI JSON ledger report.

### Configuration validation

- CLI flag validation rejects invalid flag combinations fail-closed (`susi mcp ledger --refetched <dir>` without `--drift` exits non-zero).
- Config schema validation (`susi_config::validate::validate_dir`) verifies `~/.susi/config.json` before serving and reports blocking `TypeMismatch` findings when `max_rpc_body_bytes` is non-numeric.
- `GmcpServer::run_stdio` validates `max_rpc_body_bytes` prior to reading requests: a `0` frame limit fails closed with diagnostics exclusively on `stderr` and emits zero non-JSON-RPC bytes on `stdout`.

### Bounded metrics, logs and traces

- `prompts/list` enforces a page ceiling of `100` items per page using snapshot-revision hash cursors (`"{revision}:{end}"`), and carries explicit no-cache hints (`ttlMs: 0`, `cacheScope: "private"`).
- `completion/complete` filters deterministically, bounds responses to `<= 100` items per call (`completion.values`), and sets `total` and `hasMore` accurately (`total > 100`).
- `logging/setLevel` adjusts session log verbosity cleanly without altering framing invariants; operational diagnostics write exclusively to `stderr`, keeping `stdout` strictly valid JSON-RPC frames.
- Correlation fields (`traceparent`, `_meta["io.modelcontextprotocol/protocolVersion"]`, `_meta["io.modelcontextprotocol/clientCapabilities"]`) are propagated boundedly without bypassing header/era/schema validation.

### Diagnostics and incident triage

- `-32602` (`Invalid params`): unknown prompt name, missing required prompt argument, unknown completion `ref/prompt` or `ref/resource`, malformed resource template URI, stale-revision `prompts/list` pagination cursor, or modern `2026-07-28` request missing required `_meta` keys.
- `-32600` (`Invalid Request`): modern `_meta` injected into a legacy `2025-11-25` session, malformed JSON-RPC envelope, or oversized frame rejected by `BoundedLines`.
- `-32700` (`Parse error`): malformed JSON payload rejected before dispatch.
- `-32022`: unsupported modern `_meta` protocol version (response includes `data.supported`).
- `-32024`: HTTP `2026-07-28` standard header mismatch (`Mcp-Method` or plain/RFC-2047 Base64 `Mcp-Name` disagreeing with `prompts/get` `params.name`).
- `hash_drift` / `unfetchable`: typed drift findings in `susi mcp ledger --drift --refetched <dir>` when any of the 4 pinned prompt or completion specs changes bytes or goes missing relative to `.agents/gmcp/sources.json`:
  - `docs/specification/2025-11-25/server/prompts.mdx` (`sha256: 5c4a5adefade5d9ecaa1446278a46d48c11fbfb4217bcd81c8fc004928a4abe1`)
  - `docs/specification/2025-11-25/server/utilities/completion.mdx` (`sha256: ea42d006ba3b3ab8deceb5ae22be17f79e40b26a207947459560018f91eb42e9`)
  - `docs/specification/2026-07-28/server/prompts.mdx` (`sha256: 85e550635bbbbaa90dff8738f98808c62987e8cfce1d75305b55ca43f3c546de`)
  - `docs/specification/2026-07-28/server/utilities/completion.mdx` (`sha256: 1965c563aadb99e141131444caa81ff1860ed82bab314b286c40a0f0ba839f99`)

### Examples

```text
# 1. Inspect GMCP ledger verification and MCP adapter status
susi mcp ledger
susi mcp status

# 2. Run air-gapped standards drift detection against vendored MCP prompt/completion specs
susi mcp ledger --drift --refetched .agents/gmcp/spec

# 3. Verify VC-203-011 operations and refresh current local evidence
cargo test --offline --locked --test gmcp_requirements gmcp_req_203_011_06_operations
local-release record-gmcp
local-release record-gmcp --mastery
```

### Incident triage

Treat any `verified: false` drift finding (`hash_drift` or `unfetchable`), zero-byte stdio limit startup refusal, `stdout` non-JSON frame leak, HTTP `Mcp-Name` validation bypass, or stale-cursor acceptance across `Change::Prompts` catalog mutations as a release-blocking incident. Preserve the `susi mcp ledger --drift` JSON report, `stderr` diagnostics, failing request envelope or `config.json`, and latest `.susi/records/gmcp/evidence/VC-203-*.jsonl` proof records before remediation, then re-verify `gmcp_req_203_011`.

### Migration (re-pinning the prompt and completion standards)

1. Claim an isolated task (`susi-commercial tasks claim <id>`) documenting the upstream MCP specification commit motivating any prompt or completion schema update.
2. Refresh `docs/specification/2025-11-25/server/prompts.mdx`, `2025-11-25/server/utilities/completion.mdx`, `2026-07-28/server/prompts.mdx`, and `2026-07-28/server/utilities/completion.mdx` under `.agents/gmcp/spec/` along with `.agents/gmcp/sources.json`.
3. Update `.agents/gmcp/requirements-ledger.json`, reconcile this contract table and `crates/susi-gmcp/src/{protocol,catalog,mcp/http}.rs` behavior, and re-run `gmcp_req_203_011_01..06` and `check-gmcp-roadmap`.
4. Commit the updated tests and run two-phase `local-release record-gmcp` followed by `--mastery` to produce new append-only proof records.

### Rollback

Specifications, contract prose, ledger bindings and tests are versioned Git artifacts; operational evidence uses local current snapshots. Revert a faulty prompt/completion schema or transport header change with `git revert <commit>`, re-validate `~/.susi/config.json` (`susi_config::validate::validate_dir`), verify `susi mcp ledger --drift --refetched .agents/gmcp/spec` passes cleanly, and run `gmcp_req_203_011_06_operations` plus the two-phase evidence recorder. Refresh local snapshots after the checks pass; optional history may be absent.

## Production guarantees and non-claims

- Production guarantees hold only for `crates/susi-gmcp/src/{protocol,catalog,content,jsonrpc_gate,mcp/http}.rs` behind strict schema, URI, revision-cursor, and HTTP `Mcp-Method`/`Mcp-Name` header checks verified by `gmcp_req_203_011_01..06`.
- Non-claim: the shipped `use_tool` prompt is a static template without a multi-turn `input_required` elicitation loop (`GetPromptResult.resultType = complete`); dynamic prompt additions via `GmcpService::register_prompt` are in-process only and are not dynamically editable over external MCP admin endpoints.
