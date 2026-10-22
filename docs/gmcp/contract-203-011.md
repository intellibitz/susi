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

Positive and negative fixtures for every row live in .agents/gmcp/evidence/VC-203-011.jsonl, keyed by row id; the tests consume them.

## Evidence locations

.agents/gmcp/evidence/VC-203-011.jsonl holds the retained evidence; the acceptance test binds this document to the ledger.

## Ambiguities resolved

All clauses are resolved by this table: the ledger id is the canonical name, the production path is the only allowed implementation site, and the disposition decides whether the clause is honored on the modern path or only through the version adapter.
