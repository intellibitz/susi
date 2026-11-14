# VC-203-007 contract

This contract binds every normative clause of vector VC-203-007 (GMCP-203-007-01: transport adapter SPI and embedded/in-process operation). Each row in the behavior table is normative; its identifier, level, clause and production path are pinned.

## Normative clause

A stable transport SPI MUST admit in-process, Unix-domain socket, named-pipe, WebSocket or future transports without changing protocol semantics. A common conformance harness MUST prove ordering, cancellation, limits, identity propagation, observability, shutdown and error equivalence across adapters. Experimental transports MUST be capability-negotiated and MUST NOT be presented as standard MCP. The MVP must implement the 80 pinned clauses below; every row is normative, bound to its production path, and resolved before implementation.

## Pinned sources

| repository | document | sha256 | lines |
|---|---|---|---|
| modelcontextprotocol | docs/specification/2025-11-25/basic/transports.mdx | a247fdbb3cc25c805ef43124db18d9b60a56669b3e65bd163dffb76f4129dfc0 | 7-319 |
| modelcontextprotocol | docs/specification/2026-07-28/basic/transports/index.mdx | 59cc01897f5efadc1d412007a47df4f19b30725cdeaf264bbaf25e6b6b0aeb51 | 29-75 |

## Canonical data model

Each row names a canonical behavior; its ledger id is the canonical name and its production path is the only implementation site. A transport adapter is an explicit value exposing: a transport id, a standard-or-experimental flag, negotiated capabilities, an ordered framed-message channel, a cancellation signal, a per-message size limit, the authenticated principal, and a shutdown handle. Requests, responses and handles are explicit data, never ambient state; protocol semantics live above the SPI and are identical for every adapter.

## Behavior table

| id | level | clause | production path |
|---|---|---|---|
| MCP-2025-1125-basic-transports-mdx-0007 | must | MCP uses JSON-RPC to encode messages. JSON-RPC messages **MUST** be UTF-8 encoded. | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0015 | should | Clients **SHOULD** support stdio whenever possible. | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0028 | must_not | - Messages are delimited by newlines, and **MUST NOT** contain embedded newlines. | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0029 | may | - The server **MAY** write UTF-8 strings to its standard error (`stderr`) for any | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0031 | may | - The client **MAY** capture, forward, or ignore the server's `stderr` output | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0032 | should_not | and **SHOULD NOT** assume `stderr` output indicates error conditions. | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0033 | must_not | - The server **MUST NOT** write anything to its `stdout` that is not a valid MCP message. | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0034 | must_not | - The client **MUST NOT** write anything to the server's `stdin` that is not a valid MCP | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0070 | must | The server **MUST** provide a single HTTP endpoint path (hereafter referred to as the | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0078 | must | 1. Servers **MUST** validate the `Origin` header on all incoming connections to prevent DN | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0079 | must | - If the `Origin` header is present and invalid, servers **MUST** respond with HTTP 403 Fo | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0080 | may | body **MAY** comprise a JSON-RPC _error response_ that has no `id` | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0081 | should | 2. When running locally, servers **SHOULD** bind only to localhost (127.0.0.1) rather than | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0082 | should | 3. Servers **SHOULD** implement proper authentication for all connections | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0088 | must | Every JSON-RPC message sent from the client **MUST** be a new HTTP POST request to the | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0091 | must | 1. The client **MUST** use HTTP POST to send JSON-RPC messages to the MCP endpoint. | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0092 | must | 2. The client **MUST** include an `Accept` header, listing both `application/json` and | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0094 | must | 3. The body of the POST request **MUST** be a single JSON-RPC _request_, _notification_, o | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0096 | must | - If the server accepts the input, the server **MUST** return HTTP status code 202 | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0098 | must | - If the server cannot accept the input, it **MUST** return an HTTP error status code | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0099 | may | (e.g., 400 Bad Request). The HTTP response body **MAY** comprise a JSON-RPC _error | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0101 | must | 5. If the input is a JSON-RPC _request_, the server **MUST** either | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0103 | must | `Content-Type: application/json`, to return one JSON object. The client **MUST** | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0106 | should | - The server **SHOULD** immediately send an SSE event consisting of an event | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0110 | may | server **MAY** close the _connection_ (without terminating the _SSE stream_) | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0112 | should | **SHOULD** then "poll" the SSE stream by attempting to reconnect. | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0114 | should | it **SHOULD** send an SSE event with a standard [`retry`](https://html.spec.whatwg.org/mul | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0115 | must | closing the connection. The client **MUST** respect the `retry` field, | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0117 | should | - The SSE stream **SHOULD** eventually include a JSON-RPC _response_ for the | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0119 | may | - The server **MAY** send JSON-RPC _requests_ and _notifications_ before sending the | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0120 | should | JSON-RPC _response_. These messages **SHOULD** relate to the originating client | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0122 | may | - The server **MAY** terminate the SSE stream if the [session](#session-management) | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0124 | should | - After the JSON-RPC _response_ has been sent, the server **SHOULD** terminate the | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0126 | may | - Disconnection **MAY** occur at any time (e.g., due to network conditions). | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0128 | should_not | - Disconnection **SHOULD NOT** be interpreted as the client cancelling its request. | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0129 | should | - To cancel, the client **SHOULD** explicitly send an MCP `CancelledNotification`. | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0130 | may | - To avoid message loss due to disconnection, the server **MAY** make the stream | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0135 | may | 1. The client **MAY** issue an HTTP GET to the MCP endpoint. This can be used to open an | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0138 | must | 2. The client **MUST** include an `Accept` header, listing `text/event-stream` as a | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0140 | must | 3. The server **MUST** either return `Content-Type: text/event-stream` in response to | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0144 | may | - The server **MAY** send JSON-RPC _requests_ and _notifications_ on the stream. | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0145 | should | - These messages **SHOULD** be unrelated to any concurrently-running JSON-RPC | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0147 | must_not | - The server **MUST NOT** send a JSON-RPC _response_ on the stream **unless** | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0150 | may | - The server **MAY** close the SSE stream at any time. | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0152 | should | **SHOULD** follow the same polling behavior as described for POST requests: | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0154 | may | - The client **MAY** close the SSE stream at any time. | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0158 | may | 1. The client **MAY** remain connected to multiple SSE streams simultaneously. | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0159 | must | 2. The server **MUST** send each of its JSON-RPC messages on only one of the connected | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0160 | must_not | streams; that is, it **MUST NOT** broadcast the same message across multiple streams. | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0161 | may | - The risk of message loss **MAY** be mitigated by making the stream | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0169 | may | 1. Servers **MAY** attach an `id` field to their SSE events, as described in the | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0171 | must | - If present, the ID **MUST** be globally unique across all streams within that | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0174 | should | - Event IDs **SHOULD** encode sufficient information to identify the originating | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0177 | should | or server-initiated closure), it **SHOULD** issue an HTTP GET to the MCP endpoint, | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0181 | may | - The server **MAY** use this header to replay messages that would have been sent | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0184 | must_not | - The server **MUST NOT** replay messages that would have been delivered on a | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0198 | may | 1. A server using the Streamable HTTP transport **MAY** assign a session ID at | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0201 | should | - The session ID **SHOULD** be globally unique and cryptographically secure (e.g., a | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0203 | must | - The session ID **MUST** only contain visible ASCII characters (ranging from 0x21 to | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0205 | must | - The client **MUST** handle the session ID in a secure manner, see [Session Hijacking mit | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0207 | must | the Streamable HTTP transport **MUST** include it in the `MCP-Session-Id` header on | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0209 | should | - Servers that require a session ID **SHOULD** respond to requests without an | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0211 | must | 3. The server **MAY** terminate the session at any time, after which it **MUST** respond | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0214 | must | `MCP-Session-Id`, it **MUST** start a new session by sending a new `InitializeRequest` | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0217 | should | the client application) **SHOULD** send an HTTP DELETE to the MCP endpoint with the | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0219 | may | - The server **MAY** respond to this request with HTTP 405 Method Not Allowed, | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0265 | must | If using HTTP, the client **MUST** include the `MCP-Protocol-Version: | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0271 | should | The protocol version sent by the client **SHOULD** be the one [negotiated during | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0276 | should | protocol version negotiated during initialization - the server **SHOULD** assume protocol | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0280 | must | `MCP-Protocol-Version`, it **MUST** respond with `400 Bad Request`. | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0313 | may | Clients and servers **MAY** implement additional custom transport mechanisms to suit | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0317 | must | Implementers who choose to support custom transports **MUST** ensure they preserve the | crates/susi-gmcp/src/server.rs |
| MCP-2025-1125-basic-transports-mdx-0319 | should | **SHOULD** document their specific connection establishment and message exchange patterns | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-index-mdx-0029 | must | MCP uses JSON-RPC to encode messages. JSON-RPC messages **MUST** be UTF-8 | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-index-mdx-0032 | must | A binding **MUST** deliver client-sent _requests_ and _notifications_ to the | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-index-mdx-0045 | may | A binding **MAY** additionally mirror selected body fields into envelope | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-index-mdx-0062 | may | Clients and servers **MAY** implement additional custom transport mechanisms | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-index-mdx-0067 | must | Implementers who choose to support custom transports **MUST** preserve the | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-index-mdx-0070 | should | metadata model. Custom transports **SHOULD** document their connection | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-index-mdx-0075 | should | Unix domain sockets or TCP) **SHOULD** reuse the | crates/susi-gmcp/src/server.rs |

## Compatibility and deprecation decisions

Every clause carries an explicit disposition: adapter=53, optional=22, modern=5. A clause disposed as adapter is honored only through the explicit version adapter with downgrade telemetry and a removal policy, never on the modern path. Experimental transports are never advertised as standard MCP; they appear only after capability negotiation.

## Threat model

A malicious peer can send oversized or malformed frames, reorder or replay messages, spoof another principal's identity over a local socket or pipe, stall a connection past its deadline, or claim an experimental transport is standard. Adapters enforce frame limits, preserve ordering, bind identity at connection time, honor cancellation before any side effect, and reject unnegotiated transports with a typed error.

## SLOs and resource budgets

Every interaction completes within its declared deadline or is cancelled with a typed error. Per-connection queues and frame sizes are bounded, shutdown drains within a bounded grace period, and error codes are identical across adapters, so no adapter can grow memory without limit.

Published budgets, enforced by `tests/gmcp_requirements.rs::gmcp_req_203_007_05_interop` on every adapter it serves (the test reads these lines; a budget changes here or not at all). They are ceilings for an unoptimized test build on a loaded gate host, not performance claims:

- budget: startup_ms <= 10000 (adapter opened to `initialize` answered)
- budget: request_ms <= 5000 (one `ping`, worst case)
- budget: pipeline_ms <= 60000 (400 pipelined requests answered, each exactly once)
- budget: fairness_ms <= 10000 (one `ping` on a second session while the first is flooded)
- budget: memory_growth_mib <= 256 (resident growth of the serving process across the pipelined run; measured on Linux)
- budget: disk_growth_kib <= 1024 (bytes the served sessions add under the instance home)

## Fixtures

Positive and negative fixtures for every row live in .agents/gmcp/evidence/VC-203-007.jsonl, keyed by row id; the tests consume them. The shared conformance harness runs the same fixtures against each adapter.

## Evidence locations

.agents/gmcp/evidence/VC-203-007.jsonl holds the retained evidence; the acceptance test binds this document to the ledger.

## Ambiguities resolved

All clauses are resolved by this table: the ledger id is the canonical name, the production path is the only allowed implementation site, and the disposition decides whether the clause is honored on the modern path or only through the version adapter. Transport-specific behavior (framing, handshake) is confined to the adapter and never alters protocol semantics.

## Operations and incident runbook

### Inspectable state

- `susi mcp ledger` is the production ledger verification CLI command. It emits one bounded JSON report exposing `rows`, `documents`, `revisions`, `dispositions`, `categories`, `vectors`, and `verified`; a `false` verification exits non-zero before transport traffic is trusted.
- `susi mcp status` prints the configured catalog and custom MCP server adapters along with enablement and prerequisite readiness checks.
- Every served byte-pipe transport adapter (`susi mcp serve` stdio, in-process duplex over `StdioGate` + `BoundedLines`, and Unix-domain sockets on `#[cfg(unix)]`) exposes identical live API state: `server/discover` (stateless pre-handshake identity, capability surface, protocol versions, and cache hints), `initialize` (negotiated revision and capabilities), and `tools/call ledger_verify` returning the same canonical JSON ledger report as the CLI.
- `susi mcp ledger --drift --refetched <dir>` runs deterministic, air-gapped standards drift detection against the pinned transport sources (`docs/specification/2025-11-25/basic/transports.mdx` and `docs/specification/2026-07-28/basic/transports/index.mdx`).

### Configuration validation

- CLI flag validation rejects invalid flag combinations fail-closed (`susi mcp ledger --refetched <dir>` without `--drift` exits non-zero).
- Config schema validation (`susi_config::validate::validate_dir`) checks `~/.susi/config.json` before launch and reports a blocking `TypeMismatch` if `max_rpc_body_bytes` is non-numeric or malformed.
- `GmcpServer::run_stdio` reads `max_rpc_body_bytes` before serving: a configured `0` limit fails closed with an explicit diagnostic on `stderr` and writes zero non-MCP bytes to `stdout`; an unparseable `config.json` emits a `[GMCP] Config load failed` diagnostic on `stderr` and falls back cleanly to bundled defaults.

### Bounded metrics, logs and traces

- Framing memory is strictly bounded per line by `BoundedLines` (`max_rpc_body_bytes`, default `10485760` bytes) on stdio and byte-pipe adapters, and by `StreamableHttpServerConfig::max_request_body_bytes` (plus the 16-byte AEAD tag for sealed peer channels) on HTTP; concurrent HTTP accepts are bounded by a `Semaphore` clamped to `max_concurrent_agents`.
- Startup and operational telemetry (version, `max_rpc_body_bytes=<n>`, config fallback/refusal warnings, channel failures) write exclusively to UTF-8 `stderr`; `stdout` emits only valid newline-delimited JSON-RPC frames (`MCP-2025-1125-basic-transports-mdx-0029`, `MCP-2025-1125-basic-transports-mdx-0033`).
- Correlation traces (`traceparent`, `tracestate`, `baggage`, `_meta`) remain bounded request metadata for diagnostics and never bypass framing limits, capability negotiation, or authorization gates.
- Latency and resource ceilings follow the SLO table above (`startup_ms <= 10000`, `request_ms <= 5000`, `pipeline_ms <= 60000`, `fairness_ms <= 10000`, `memory_growth_mib <= 256`, `disk_growth_kib <= 1024`).

### Diagnostics and incident triage

- Oversized frames exceeding `max_rpc_body_bytes` trigger `BoundedLines` -> `StdioGate` recovery: the server emits one typed JSON-RPC `-32600` (`Invalid request: message exceeds the configured byte limit`) response per oversized line and immediately continues serving subsequent valid requests sharing the same read burst.
- Malformed JSON lines emit `-32700` (`Parse error`); batch arrays, missing envelopes, or zero/non-positive invalid frames emit `-32600`; unsupported modern `_meta` protocol versions emit `-32022`.
- Drift detection findings are typed: `hash_drift` indicates bytes changed relative to the pinned SHA-256 (`a247fdbb...` or `59cc0189...`), and `unfetchable` indicates a missing or unreadable source document. Either finding sets `verified: false` and exits non-zero.

### Examples

```text
# 1. Inspect ledger verification and MCP adapter status
susi mcp ledger
susi mcp status

# 2. Run deterministic air-gapped drift check against vendored transport specs
susi mcp ledger --drift --refetched .agents/gmcp/spec

# 3. Serve stdio with an explicit frame bound (~/.susi/config.json)
{"max_rpc_body_bytes": 1024}
susi mcp serve

# 4. Run the transport operations gate and refresh reproducible evidence
cargo test --offline --locked --test gmcp_requirements gmcp_req_203_007_06_operations
bash scripts/gmcp-record-evidence.sh
```

### Incident triage

Treat any `verified: false` drift finding (`hash_drift` or `unfetchable`), zero-limit startup refusal on `stderr`, or non-JSON frame leak on `stdout` as a release-blocking incident. Preserve the CLI JSON output, server `stderr` diagnostic log, offending input frame or `config.json`, and latest `.agents/gmcp/evidence/VC-203-*.jsonl` records before mutating state. Verify adapter parity across `susi mcp serve` and in-process `StdioGate` + `BoundedLines` via `cargo test --offline --locked --test gmcp_requirements gmcp_req_203_007_06_operations`.

### Migration (re-pinning the transport SPI standard)

1. Claim a task (`susi-commercial tasks claim <id>`) and record the upstream specification revision and reason for updating `2025-11-25/basic/transports.mdx` or `2026-07-28/basic/transports/index.mdx`.
2. Refresh `.agents/gmcp/spec/` and `.agents/gmcp/sources.json` from the exact upstream commit.
3. Reconcile `.agents/gmcp/requirements-ledger.json` (`scripts/check-gmcp-ledger.py --write`), update this contract's behavior table and adapter parity tests (`tests/gmcp_requirements.rs`), and keep protocol semantics identical across adapters (`Transport` / `IntoTransport`, `StdioGate`, `BoundedLines`).
4. Verify `susi mcp ledger`, `susi mcp ledger --drift --refetched .agents/gmcp/spec`, and the full `gmcp_requirements` suite, then regenerate signed evidence with `scripts/gmcp-record-evidence.sh`.

### Rollback

The vendored transport specs, ledger, fixtures, contract, and evidence logs are versioned Git artifacts. Revert a broken re-pin or framing limit regression with `git revert <commit>`, validate `~/.susi/config.json` (`susi_config::validate::validate_dir`), and re-run `cargo test --locked --test gmcp_requirements gmcp_req_203_007_06_operations` plus the evidence recorder; never delete historical JSONL evidence lines or rewrite prior proof records.

## Production guarantees and non-claims

- **Production transport adapters on a single protocol service**: `GmcpServer::run_stdio`, `ChannelConnection` / `serve_channel_connection`, Unix-domain socket `Transport` (`#[cfg(unix)]`), and `http_service` drive the shared `GmcpService` behind `StdioGate` (`crates/susi-gmcp/src/jsonrpc_gate.rs`) and `BoundedLines` (`crates/susi-gmcp/src/stdio.rs`).
- **Framing, limits, and fail-closed startup**: `max_rpc_body_bytes` is validated before opening stdio transport (`0` refuses startup with zero stdout bytes and emits an explicit stderr diagnostic), oversized frames are rejected with typed JSON-RPC `-32600` without breaking subsequent requests, and torn frames or dropped pipes end sessions cleanly without leaking across concurrent sessions.
- **Cross-adapter parity and wire conformance**: Direct `classify_line`, HTTP `http_envelope_rejection`, live in-process (`embedded_adapter`), Unix-domain socket (`#[cfg(unix)]`), and `susi mcp serve` produce identical verdicts across `2025-11-25` legacy handshake and `2026-07-28` modern-era fixtures and pipelined property runs, bounded by published latency, fairness, startup, and disk budgets.
- **Not covered:** Server role only on byte-pipe SPI (`Transport`/`IntoTransport`, stdio, in-process channel, Unix-domain socket where `#[cfg(unix)]`) and Streamable HTTP (`crates/susi-gmcp/src/server.rs`). Client transport role, Windows named pipes, and WebSocket transport adapters have no implementation in core and are not claimed; process RSS statm ceiling checks run only on Linux targets.
