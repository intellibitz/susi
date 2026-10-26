# VC-203-009 contract

This contract binds every normative clause of vector VC-203-009. Each row in the behavior table is normative; its identifier, level, clause and production path are pinned.

## Normative clause

The MVP must implement the 67 pinned clauses below. Every row is normative, bound to its production path, and resolved before implementation.

## Pinned sources

| repository | document | sha256 | lines |
|---|---|---|---|
| modelcontextprotocol | docs/specification/2025-11-25/server/tools.mdx | 39e56ad4f3d1ff1cb28ee62283e02947cd97db8aa6190782d629f4562a0f354c | 24-518 |
| modelcontextprotocol | docs/specification/2026-07-28/server/tools.mdx | ed550806a58eb7744b858fb9f26001aa5e55dac9e2babe88317cc81d9d4c490d | 16-795 |

## Canonical data model

Each row names a canonical behavior; its ledger id is the canonical name and its production path is the only implementation site. Requests, responses and handles are explicit data, never ambient state.

## Behavior table

| id | level | clause | production path |
|---|---|---|---|
| MCP-2025-1125-server-tools-mdx-0024 | should | For trust & safety and security, there **SHOULD** always | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2025-1125-server-tools-mdx-0027 | should | Applications **SHOULD**: | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2025-1125-server-tools-mdx-0038 | must | Servers that support tools **MUST** declare the `tools` capability: | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2025-1125-server-tools-mdx-0153 | should | capability **SHOULD** send a notification: | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2025-1125-server-tools-mdx-0201 | must | - **MUST** be a valid JSON Schema object (not `null`) | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2025-1125-server-tools-mdx-0213 | must | For trust & safety and security, clients **MUST** consider tool annotations to | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2025-1125-server-tools-mdx-0219 | should | - Tool names **SHOULD** be between 1 and 128 characters in length (inclusive). | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2025-1125-server-tools-mdx-0220 | should | - Tool names **SHOULD** be considered case-sensitive. | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2025-1125-server-tools-mdx-0221 | should | - The following **SHOULD** be the only allowed characters: uppercase and lowercase ASCII letters (A-Z, a-z), digits | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2025-1125-server-tools-mdx-0223 | should_not | - Tool names **SHOULD NOT** contain spaces, commas, or other special characters. | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2025-1125-server-tools-mdx-0224 | should | - Tool names **SHOULD** be unique within a server. | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2025-1125-server-tools-mdx-0279 | may | A tool **MAY** return links to [Resources](/specification/2025-11-25/server/resources), to provide additional context | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2025-1125-server-tools-mdx-0301 | may | [Resources](/specification/2025-11-25/server/resources) **MAY** be embedded to provide additional context | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2025-1125-server-tools-mdx-0302 | should | or data using a suitable [URI scheme](./resources#common-uri-schemes). Servers that use embedded resources **SHOULD** implement the `resources` capability: | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2025-1125-server-tools-mdx-0326 | should | For backwards compatibility, a tool that returns structured content SHOULD also return the serialized JSON in a TextContent block. | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2025-1125-server-tools-mdx-0340 | must | - Servers **MUST** provide structured results that conform to this schema. | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2025-1125-server-tools-mdx-0341 | should | - Clients **SHOULD** validate structured results against this schema. | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2025-1125-server-tools-mdx-0476 | should | Clients **SHOULD** provide tool execution errors to language models to enable self-correction. | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2025-1125-server-tools-mdx-0477 | may | Clients **MAY** provide protocol errors to language models, though these are less likely to result in successful recovery. | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2025-1125-server-tools-mdx-0512 | must | 1. Servers **MUST**: | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2025-1125-server-tools-mdx-0518 | should | 2. Clients **SHOULD**: | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0016 | must | `io.modelcontextprotocol/clientCapabilities`). Every request **MUST** include | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0033 | should | For trust & safety and security, there **SHOULD** always | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0036 | should | Applications **SHOULD**: | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0047 | must | Servers that support tools **MUST** declare the `tools` capability: | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0062 | must | Servers that declare the `tools` capability **MUST** respond to `tools/list` requests | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0063 | may | with the set of tools currently available to the requesting client. This set **MAY** be | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0064 | may | empty and **MAY** change over time (see | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0065 | must_not | [List Changed Notification](#list-changed-notification)), but **MUST NOT** vary | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0067 | may | **MAY** vary by the authorization presented on the request — for example, returning | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0071 | should | Servers **SHOULD** return tools in a deterministic order (i.e., the same ordering across | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0176 | may | Servers **MAY** respond to `tools/call` with an [`InputRequiredResult`](/specification/2026-07-28/basic/patterns/mrtr#inputrequiredresult) to indicate that additional input is needed before the tool call can be completed. This follows the [ | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0234 | must | Note that the JSON-RPC `id` **MUST** be different between the initial request and the retry. | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0239 | should | capability **SHOULD** send a notification to clients that have opened a | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0293 | must | - **MUST** be a valid JSON Schema object (not `null`) | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0297 | may | - Properties **MAY** include an [`x-mcp-header`](#x-mcp-header) annotation to expose | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0305 | must | For trust & safety and security, clients **MUST** consider tool annotations to | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0311 | should | - Tool names **SHOULD** be between 1 and 128 characters in length (inclusive). | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0312 | should | - Tool names **SHOULD** be considered case-sensitive. | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0313 | should | - The following **SHOULD** be the only allowed characters: uppercase and lowercase ASCII letters (A-Z, a-z), digits | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0315 | should_not | - Tool names **SHOULD NOT** contain spaces, commas, or other special characters. | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0316 | should | - Tool names **SHOULD** be unique within a server. | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0325 | may | aggregate tools from multiple servers **MAY** encounter naming collisions (for | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0326 | should | example, two servers each exposing a `search` tool) and **SHOULD** implement a | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0330 | should_not | servers and **SHOULD NOT** be relied upon for disambiguation. | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0348 | must_not | - **MUST NOT** be empty | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0349 | must | - **MUST** match HTTP field-name token syntax (`1*tchar`, [RFC 9110 Section 5.1](https://datatracker.ietf.org/doc/html/rfc9110#section-5.1)) | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0350 | must_not | - **MUST NOT** contain control characters, including carriage return (CR, `\r`) or | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0352 | must | - **MUST** be case-insensitively unique among all `x-mcp-header` values in the | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0354 | must | - **MUST** only be applied to parameters with primitive types (integer, string, boolean). | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0355 | must | Parameters with type `number` are not permitted. Integer values **MUST** be within the | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0357 | must | - **MUST** only be applied to properties that are _statically reachable_ from the schema | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0362 | must | Clients using the Streamable HTTP transport **MUST** reject tool definitions where any | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0363 | must | `x-mcp-header` value violates these constraints. Rejection means the client **MUST** | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0364 | should | exclude the invalid tool from the result of `tools/list`. Clients **SHOULD** log a | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0367 | may | valid tools from being used. Clients using other transports (e.g., stdio) **MAY** ignore | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0399 | should_not | Server developers **SHOULD NOT** mark sensitive parameters (passwords, API keys, tokens, | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0453 | may | A tool **MAY** return links to [Resources](/specification/2026-07-28/server/resources), to provide additional context | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0475 | may | [Resources](/specification/2026-07-28/server/resources) **MAY** be embedded to provide additional context | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0476 | should | or data using a suitable [URI scheme](./resources#common-uri-schemes). Servers that use embedded resources **SHOULD** implement the `resources` capability: | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0500 | should | For backwards compatibility, a tool that returns structured content SHOULD also return the serialized JSON in a TextContent block. | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0514 | must | - Servers **MUST** provide structured results that conform to this schema. | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0515 | should | - Clients **SHOULD** validate structured results against this schema. | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0784 | may | Clients **MAY** provide protocol errors to language models, though these are less likely to result in successful recovery. | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0785 | should | Clients **SHOULD** provide tool execution errors to language models to enable self-correction. | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0789 | must | 1. Servers **MUST**: | crates/susi-gmcp/src/tool_registry.rs |
| MCP-2026-0728-server-tools-mdx-0795 | should | 2. Clients **SHOULD**: | crates/susi-gmcp/src/tool_registry.rs |

## Compatibility and deprecation decisions

Every clause carries an explicit disposition: adapter=18, modern=36, optional=13. A clause disposed as adapter is honored only through the explicit version adapter with downgrade telemetry and a removal policy.

## Threat model

A malicious peer can replay a handle, answer another principal's request, or stall past a deadline. Handles are principal-scoped and single-use; responses are validated against the request that created them; deadlines and cancellation are enforced before any side effect.

## SLOs and resource budgets

Every interaction completes within its declared deadline or is cancelled with a typed error. Bounded queues and per-principal handle budgets prevent unbounded growth.

## Fixtures

Positive and negative fixtures for every row live in .agents/gmcp/evidence/VC-203-009.jsonl, keyed by row id; the tests consume them.

## Evidence locations

.agents/gmcp/evidence/VC-203-009.jsonl holds the retained evidence; the acceptance test binds this document to the ledger.

## Ambiguities resolved

All clauses are resolved by this table: the ledger id is the canonical name, the production path is the only allowed implementation site, and the disposition decides whether the clause is honored on the modern path or only through the version adapter.

## Operations and incident runbook

### Inspectable state

- `susi mcp ledger` emits the bounded JSON status report for all bound specifications and ledger rows including the 67 tool server surface rows of VC-203-009.
- Tool server catalog state is inspectable via standard `tools/list` over stdio and HTTP transports, returning canonical tool names, deterministic ordering, and JSON Schema input specifications.
- In-process execution state is inspectable via `call_tool_direct` and `ToolRegistry::list_tools` across registered custom and built-in tools.
- `susi mcp ledger --drift --refetched <dir>` is the deterministic, air-gapped drift detection check for server tool specifications.

### Diagnostics and incident triage

Every drift finding has a stable `kind`: `hash_drift` means bytes differ from the pinned source, while `unfetchable` means a required document is absent or could not be retrieved. A non-zero command and `verified: false` are required for either condition; callers must not continue with a partially checked corpus. For a local failure, restore the named vendored file from git. For an upstream `hash_drift`, preserve the report and begin the migration below.
Protocol errors on tool calls are strictly typed: unknown tools earn `-32601` or typed error responses, schema violations earn inputSchema failure with `isError: true`, malformed parameter envelopes earn `-32600` or `-32602` (invalid params), and unsupported protocol versions earn `-32022`. Diagnostic traces write strictly to server error logs while responses remain clean JSON.

### Incident triage

Treat a non-zero report or schema validation failure as an incident until the tool definition or handler contract is restored and the deterministic check is green again. Preserve the JSON report and the corresponding evidence record with the incident for independent reproduction.

### Migration (re-pinning server-tools specification)

1. Claim a task and record the upstream revision and reason for the re-pin.
2. Refresh `.agents/gmcp/spec/` and `sources.json` from that exact revision.
3. Run `scripts/check-gmcp-ledger.py --write`, review every changed server-tools clause, and update fixtures, compatibility decisions, and the retained evidence record.
4. Run `susi mcp ledger`, the deterministic `--refetched` check, and the full GMCP acceptance gate before publishing the change.

### Rollback

The vendored specification, manifest, ledger, fixtures, and evidence are committed artifacts. Roll back a bad re-pin with `git revert` of its commit, then rerun the CLI and deterministic drift checks. This restores the prior server-tools semantics without a data migration; do not delete evidence rows or rewrite a prior proof event.

