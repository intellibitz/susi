# VC-203-006 contract

This contract binds every normative clause of vector VC-203-006. Each row in the behavior table is normative; its identifier, level, clause and production path are pinned.

## Normative clause

The MVP must implement the 80 pinned clauses below. Every row is normative, bound to its production path, and resolved before implementation.

## Pinned sources

| repository | document | sha256 | lines |
|---|---|---|---|
| modelcontextprotocol | docs/specification/2026-07-28/basic/transports/streamable-http.mdx | 22574bf11e004068493787203ce92be1162107cad717bcb805a15780d4fa69c9 | 47-704 |
| susi | docs/gmcp-roadmap.md | 30a6061c099a4a504ad9fcfb42f31a20d2fa332288a9cbd83f81080c0fd6be54 | 103-103 |

## Canonical data model

Each row names a canonical behavior; its ledger id is the canonical name and its production path is the only implementation site. Requests, responses and handles are explicit data, never ambient state.

## Behavior table

| id | level | clause | production path |
|---|---|---|---|
| CLAIM-02 | must | bounded stdio and Streamable HTTP, local auth, origin/host checks, rate and concurrency controls, TLS for remote use | crates/susi-gmcp/src |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0047 | must | The server **MUST** provide a single HTTP endpoint path (hereafter referred to | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0058 | must | 1. Servers **MUST** validate the `Origin` header on all incoming connections | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0060 | must | - If the `Origin` header is present and invalid, servers **MUST** respond | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0061 | may | with HTTP 403 Forbidden. The HTTP response body **MAY** comprise a | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0063 | should | 2. When running locally, servers **SHOULD** bind only to localhost | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0065 | should | 3. Servers **SHOULD** implement proper authentication for all connections. | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0072 | must | Every JSON-RPC message sent from the client **MUST** be a new HTTP POST | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0075 | must | 1. The client **MUST** use HTTP POST to send JSON-RPC messages. | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0076 | must | 2. The client **MUST** include an `Accept` header listing both | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0078 | must | 3. The client **MUST** include the [request metadata headers](#request-metadata) | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0080 | must | 4. The body of the HTTP POST **MUST** be a single JSON-RPC _request_ or | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0081 | must_not | _notification_. The client **MUST NOT** send JSON-RPC _responses_. | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0083 | must | - If the server accepts it, the server **MUST** return HTTP status code | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0085 | must | - If the server cannot accept it, it **MUST** return an HTTP error status | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0086 | may | code (e.g., `400 Bad Request`). The HTTP response body **MAY** comprise | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0088 | must | 6. If the body is a JSON-RPC _request_, the server **MUST** return either | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0091 | must | **MUST** support both. | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0112 | may | - The server **MAY** send JSON-RPC _notifications_ — for example, | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0115 | must | before the final response. These notifications **MUST** relate to the | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0117 | must_not | - The server **MUST NOT** send independent JSON-RPC _requests_ on this stream. | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0125 | should | - The final JSON-RPC _response_ **SHOULD** terminate the stream. | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0136 | should | When initiating an SSE stream, servers **SHOULD** include the | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0235 | must | Closing the SSE response stream **MUST** be treated by the server as | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0237 | should | stream, the transport-level disconnect is unambiguous. The server **SHOULD** | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0238 | must_not | stop work on the cancelled request as soon as practical and **MUST NOT** send | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0252 | must | Every POST request to the MCP endpoint **MUST** include an | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0257 | must | The header value **MUST** match the | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0259 | must | `_meta`. If the values do not match, the server **MUST** reject the request | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0265 | must | not to support), it **MUST** respond with `400 Bad Request` and an | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0271 | must | If the server does not implement the requested RPC method, it **MUST** respond | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0278 | may | `2025-06-18` (which did not define the `MCP-Protocol-Version` header) **MAY** | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0280 | must | server that does not support such clients **MUST** reject a request without | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0293 | must | These headers are **REQUIRED** for compliance. | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0296 | must | header value, clients **MUST** encode it using the Base64 sentinel format | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0358 | may | MCP servers **MAY** designate specific tool parameters to be mirrored into | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0364 | must | While the use of `x-mcp-header` is optional for servers, clients **MUST** | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0366 | must | `x-mcp-header` annotations, conforming clients **MUST** mirror the | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0378 | must_not | - **MUST NOT** be empty | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0379 | must | - **MUST** match HTTP field-name token syntax (`1*tchar`, [RFC 9110 Section 5.1](https://datatracker.ietf.org/doc/html/rfc9110#section-5.1)) | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0380 | must_not | - **MUST NOT** contain control characters, including carriage return (CR, `\r`) | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0382 | must | - **MUST** be case-insensitively unique among all `x-mcp-header` values in | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0384 | must | - **MUST** only be applied to parameters with primitive types (integer, | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0386 | must | Integer values **MUST** be within the safe range for JavaScript | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0388 | must | - **MUST** only be applied to properties that are _statically reachable_ | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0390 | must_not | `properties` keys. The chain **MUST NOT** pass through `items` (or any | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0402 | must | Clients using the Streamable HTTP transport **MUST** reject tool definitions | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0404 | must | the client **MUST** exclude the invalid tool from the result of `tools/list`. | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0405 | should | Clients **SHOULD** log a warning when rejecting a tool definition, including | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0408 | may | Clients using other transports (e.g., stdio) **MAY** ignore `x-mcp-header` | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0469 | must | Clients **MUST** encode parameter values before including them in HTTP | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0483 | must | whitespace), clients **MUST** use Base64 encoding of the UTF-8 | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0491 | should | prompt names are only **SHOULD**-constrained to header-safe characters, so a | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0499 | must | Base64-encoded. These markers are case-sensitive and **MUST** appear exactly | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0501 | must | values **MUST** decode them accordingly. In particular, servers **MUST** | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0506 | must | To avoid ambiguity, clients **MUST** also Base64-encode any plain-ASCII | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0525 | must | **MUST**: | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0542 | should | **SHOULD** call `tools/list` to check for changes to the tool's | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0549 | must | **MUST** forward it and otherwise ignore it, as required by the | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0552 | must | Servers **MUST** reject requests with a recognized `Mcp-Param-{Name}` header | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0555 | must | Any server that processes the message body **MUST** validate that encoded | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0557 | must | values in the request body. Servers **MUST** reject requests with a | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0563 | must | \| Parameter value provided \| Client MUST include the header \| Server MUST validate header matches body \| | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0564 | must_not | \| Parameter value is `null` \| Client MUST omit the header \| Server MUST NOT expect the header \| | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0565 | must_not | \| Parameter not in arguments \| Client MUST omit the header \| Server MUST NOT expect the header \| | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0566 | must | \| Client omits header but value is in body \| Non-conforming client \| Server MUST reject the request \| | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0574 | must | are case-insensitive. Clients and servers **MUST** use case-insensitive | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0582 | must | Servers that process the request body **MUST** reject requests where the | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0591 | should | When validating integer parameter values, servers **SHOULD** compare the | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0597 | must | When rejecting a request due to header validation failure, servers **MUST** | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0598 | must | return HTTP status `400 Bad Request` and **MUST** include a JSON-RPC error | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0628 | must | `Mcp-Param-{Name}`), servers **MUST** decode encoded values (see | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0634 | must | Intermediaries **MUST** return an appropriate HTTP error status (e.g., | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0643 | should | or rate-limiting by tenant) **SHOULD** verify that the `MCP-Protocol-Version` | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0645 | should | version is older or the header is absent, the intermediary **SHOULD** reject | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0653 | may | legacy version that requires an `initialize` handshake **MAY** detect which | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0655 | should | `400 Bad Request`, the client **SHOULD** inspect the response body before | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0681 | should | older client **SHOULD** respond as follows: | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0703 | should_not | New implementations **SHOULD NOT** adopt it; existing implementations | crates/susi-gmcp/src/server.rs |
| MCP-2026-0728-basic-transports-streamable-http-mdx-0704 | should | **SHOULD** migrate to [Streamable | crates/susi-gmcp/src/server.rs |

## Compatibility and deprecation decisions

Every clause carries an explicit disposition: modern=73, optional=7. A clause disposed as adapter is honored only through the explicit version adapter with downgrade telemetry and a removal policy.

## Threat model

A malicious peer can replay a handle, answer another principal's request, or stall past a deadline. Handles are principal-scoped and single-use; responses are validated against the request that created them; deadlines and cancellation are enforced before any side effect.

## SLOs and resource budgets

Every interaction completes within its declared deadline or is cancelled with a typed error. Bounded queues and per-principal handle budgets prevent unbounded growth.

## Fixtures

Positive and negative fixtures for every row live in .agents/gmcp/evidence/VC-203-006.jsonl, keyed by row id; the tests consume them.

## Evidence locations

.agents/gmcp/evidence/VC-203-006.jsonl holds the retained evidence; the acceptance test binds this document to the ledger.

## Ambiguities resolved

All clauses are resolved by this table: the ledger id is the canonical name, the production path is the only allowed implementation site, and the disposition decides whether the clause is honored on the modern path or only through the version adapter.
