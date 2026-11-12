# VC-203-010 contract

This contract binds every normative clause of vector VC-203-010. Each row in the behavior table is normative; its identifier, level, clause and production path are pinned.

## Normative clause

The MVP must implement the 33 pinned clauses below. Every row is normative, bound to its production path, and resolved before implementation.

## Pinned sources

| repository | document | sha256 | lines |
|---|---|---|---|
| modelcontextprotocol | docs/specification/2025-11-25/server/resources.mdx | 9c1aa45ee31c1e0f097c5d1f6316e796f0ee2d393fbc960be400e0f77cf82843 | 32-412 |
| modelcontextprotocol | docs/specification/2026-07-28/server/resources.mdx | 10538119019af5f49a2fbc55d5818725175863115dfb273e79ed7f65a18aaf0c | 17-434 |

## Canonical data model

Each row names a canonical behavior; its ledger id is the canonical name and its production path is the only implementation site. Requests, responses and handles are explicit data, never ambient state.

## Behavior table

| id | level | clause | production path |
|---|---|---|---|
| MCP-2025-1125-server-resources-mdx-0032 | must | Servers that support resources **MUST** declare the `resources` capability: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-server-resources-mdx-0212 | should | capability **SHOULD** send a notification: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-server-resources-mdx-0357 | should | Servers **SHOULD** use this scheme only when the client is able to fetch and load the | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-server-resources-mdx-0361 | should | For other use cases, servers **SHOULD** prefer to use another URI scheme, or define a | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-server-resources-mdx-0370 | may | MCP servers **MAY** identify file:// resources with an | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-server-resources-mdx-0381 | must | Custom URI schemes **MUST** be in accordance with [RFC3986](https://datatracker.ietf.org/doc/html/rfc3986), | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-server-resources-mdx-0386 | should | Servers **SHOULD** return standard JSON-RPC errors for common failure cases: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-server-resources-mdx-0409 | must | 1. Servers **MUST** validate all resource URIs | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-server-resources-mdx-0410 | should | 2. Access controls **SHOULD** be implemented for sensitive resources | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-server-resources-mdx-0411 | must | 3. Binary data **MUST** be properly encoded | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-server-resources-mdx-0412 | should | 4. Resource permissions **SHOULD** be checked before operations | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-resources-mdx-0017 | must | `io.modelcontextprotocol/clientCapabilities`). Every request **MUST** include | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-resources-mdx-0041 | must | Servers that support resources **MUST** declare the `resources` capability: | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-resources-mdx-0074 | must | Servers that declare the `resources` capability **MUST** respond to `resources/list` | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-resources-mdx-0076 | may | **MAY** be empty and **MAY** change over time (see | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-resources-mdx-0077 | must_not | [List Changed Notification](#list-changed-notification)), but **MUST NOT** vary | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-resources-mdx-0079 | may | **MAY** vary by the authorization presented on the request — for example, returning | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-resources-mdx-0173 | may | Servers **MAY** return multiple resource contents in response to a single | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-resources-mdx-0177 | may | Servers **MAY** also respond to `resources/read` with an [`InputRequiredResult`](/specification/2026-07-28/basic/patterns/mrtr#inputrequiredresult) to indicate that additional input is needed before the resource can be read. This follows th | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-resources-mdx-0235 | should | capability **SHOULD** send a notification: | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-resources-mdx-0375 | should | Servers **SHOULD** use this scheme only when the client is able to fetch and load the | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-resources-mdx-0379 | should | For other use cases, servers **SHOULD** prefer to use another URI scheme, or define a | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-resources-mdx-0388 | may | MCP servers **MAY** identify file:// resources with an | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-resources-mdx-0399 | must | Custom URI schemes **MUST** be in accordance with [RFC3986](https://datatracker.ietf.org/doc/html/rfc3986), | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-resources-mdx-0404 | must | If the requested resource does not exist, servers **MUST** return a JSON-RPC error with | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-resources-mdx-0405 | should | code `-32602` (Invalid Params). Servers **SHOULD** return `-32603` for internal errors. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-resources-mdx-0407 | should | For backwards compatibility, clients **SHOULD** also accept `-32002` as a | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-resources-mdx-0410 | must_not | Servers **MUST NOT** return an empty `contents` array for a non-existent resource. An empty array is ambiguous—it could mean the resource exists but has no content, or that it doesn't exist at all. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-resources-mdx-0430 | must | 1. Servers **MUST** validate all resource URIs | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-resources-mdx-0431 | should | 2. Access controls **SHOULD** be implemented for sensitive resources | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-resources-mdx-0432 | must | 3. Binary data **MUST** be properly encoded | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-resources-mdx-0433 | should | 4. Resource permissions **SHOULD** be checked before operations | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-resources-mdx-0434 | must | 5. Servers **MUST** sanitize file paths to prevent directory traversal attacks | crates/susi-gmcp/src/protocol.rs |

## Resource surface behavior

What the production path (`GmcpService` in crates/susi-gmcp/src/protocol.rs) does for any registered resource or template, proven on the wire by crates/susi-gmcp/src/tests/resource_surface.rs and by `gmcp_vc_203_010_mastery`:

- **Admission.** Every `resources/read` and `resources/subscribe` first checks the URI: an absolute RFC 3986 URI of at most 2048 bytes, with no whitespace or control character and no `..` path segment, plain or percent-encoded (`%2e`, `%2f`, `%5c`). A refused URI earns `-32602` and never reaches a provider.
- **Access control.** An embedder sets one guard with `set_resource_guard`; it runs after admission and before the provider, for reads and subscriptions alike, and its error is the answer. Core ships no policy of its own.
- **Sealed reads.** A provider's result is checked before it is served: `contents` is never empty, every `blob` is strict base64, every `mimeType` is well formed, and the payload is at most `MAX_RESOURCE_BYTES` (8 MiB). A read that fails any of these earns `-32603`; it is never truncated or repaired. A read is one bounded response; larger resources are refused, not streamed.
- **Not found.** An unknown resource earns a typed error that names the URI, never an empty `contents` array.
- **Cache hints.** Reads and listings carry `ttlMs` and `cacheScope`; a provider's own values are kept.
- **Listing.** `resources/list` and `resources/templates/list` page deterministically; a cursor is bound to the catalog it was issued for and is refused with `-32602` once the catalog changes.
- **Change signals.** Registering or replacing a resource announces `notifications/resources/list_changed`; a replaced or updated resource announces `notifications/resources/updated` to sessions subscribed to that URI and to no others.
- **Concurrency.** A resource replaced while it is being read is always served whole, from one version.
- **Cancellation.** A cancelled read stops its provider; the session keeps serving.

Not covered: filtering of listings beyond paging (the pinned revisions define none), and per-principal authorization inside core (the guard is the embedder's decision point).

## Compatibility and deprecation decisions

Every clause carries an explicit disposition: adapter=10, modern=17, optional=6. A clause disposed as adapter is honored only through the explicit version adapter with downgrade telemetry and a removal policy.

## Threat model

A malicious peer can send traversal or malformed URIs to reach data outside a provider's scope, request a resource it may not read, or stall a read; a faulty provider can return malformed binary, an ambiguous empty result or an unbounded payload. Admission, the guard and the sealed read above answer each. A malicious peer can also replay a handle, answer another principal's request, or stall past a deadline. Handles are principal-scoped and single-use; responses are validated against the request that created them; deadlines and cancellation are enforced before any side effect.

## SLOs and resource budgets

Every interaction completes within its declared deadline or is cancelled with a typed error. Bounded queues and per-principal handle budgets prevent unbounded growth.

## Fixtures

Positive and negative fixtures for every row live in .agents/gmcp/evidence/VC-203-010.jsonl, keyed by row id; the tests consume them.

## Evidence locations

.agents/gmcp/evidence/VC-203-010.jsonl holds the retained evidence; the acceptance test binds this document to the ledger.

## Ambiguities resolved

All clauses are resolved by this table: the ledger id is the canonical name, the production path is the only allowed implementation site, and the disposition decides whether the clause is honored on the modern path or only through the version adapter.
