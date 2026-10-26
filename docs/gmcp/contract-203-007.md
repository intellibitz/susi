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

## Fixtures

Positive and negative fixtures for every row live in .agents/gmcp/evidence/VC-203-007.jsonl, keyed by row id; the tests consume them. The shared conformance harness runs the same fixtures against each adapter.

## Evidence locations

.agents/gmcp/evidence/VC-203-007.jsonl holds the retained evidence; the acceptance test binds this document to the ledger.

## Ambiguities resolved

All clauses are resolved by this table: the ledger id is the canonical name, the production path is the only allowed implementation site, and the disposition decides whether the clause is honored on the modern path or only through the version adapter. Transport-specific behavior (framing, handshake) is confined to the adapter and never alters protocol semantics.