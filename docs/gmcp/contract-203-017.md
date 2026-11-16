# Contract GMCP-203-017-01 — Complete MCP client and host

Status: **contract pinned** (requirement `GMCP-203-017-01`, vector `VC-203-017`)
Baseline date: **2026-11-16**

## Normative clause

GMCP is a full MCP host, not only a tool caller: one unified Rust API and `susi mcp`
CLI connect to any supported MCP server (stateless `2026-07-28` Streamable HTTP,
stateful `2025-11-25` / `2025-06-18` / `2025-03-26` Streamable HTTP, stdio child
and duplex pipes, Unix domain sockets on `#[cfg(unix)]`, and in-process embedded
`GmcpService` / `TestHarness` handles), discover capabilities, call tools, read
and subscribe to resources, get prompts and argument completions, handle
server-to-client MRTR callbacks (`roots`, `sampling`, `elicitation`), drive
durable Official Tasks (`io.modelcontextprotocol/tasks`), negotiate and inspect
Official MCP Apps (`io.modelcontextprotocol/apps` and `ui://` resources), enforce
per-server authorization and credential isolation, deliver bounded notifications
and progress frames, and issue cooperative cancellation. Multiple servers operate
concurrently under one host with strictly isolated identities, `MacPolicy`
permissions, bearer credentials, session/version state, subscriptions, and callback
policies, and every host primitive and callback surface is proven against real
shipped GMCP reference servers rather than mock-only stubs.

Per `docs/gmcp-roadmap.md` (`VC-203-017`, `CLAIM-04`) and zero-UI plan A9, Susi
Core ships the complete headless protocol host, session/transport engine, callback
broker, and `susi mcp` CLI with zero third-party MCP SDK crates (`susi-vendor-mcp`
and `rmcp` are permanently retired from Core), while interactive WebView/graphical
Apps rendering surfaces live in Susi Commercial Apps.

The contract binds eight normative obligations across `VC-203-017`
(`T-CODEXGMCPROADMAP-114` through `T-CODEXGMCPROADMAP-120`):

1. **Unified multi-transport host and CLI surface.** One coherent host substrate
   (`susi_wire_client::mcp::Session` + `McpHost` / `susi_core::mcp_client` +
   `susi_tools::GmcpClient` + `src/cli/mcp_cli.rs`) connects to Streamable HTTP
   endpoints (`2026-07-28` stateless `server/discover` and `2025-11-25`..`2025-03-26`
   stateful `initialize` + `notifications/initialized` + `DELETE`), stdio child
   processes (`crates/susi-tools/src/mcp_budget.rs`), Unix sockets (`#[cfg(unix)]`),
   and in-process direct/duplex servers (`crates/susi-gmcp/src/direct_call.rs`,
   `crates/susi-gmcp/src/sdk.rs`). Every transport feeds the same JSON-RPC 2.0
   framing, negotiated capability store, and error taxonomy.
2. **Complete server primitive coverage (discover, tools, resources, subscriptions,
   prompts, completions).** Host sessions discover and retain `serverInfo`,
   `supportedVersions`, `capabilities`, and `capabilities.extensions`; list and
   invoke tools with bounded cursor pagination (`MAX_TOOLS_LIST_PAGES = 50`) and
   `CallToolResult` (`content`, `structuredContent`, `isError`) fidelity; list
   resources (`resources/list`) and RFC 6570 templates (`resources/templates/list`),
   read text/blob contents (`resources/read`), and manage filtered subscriptions
   (`subscriptions/listen`, `resources/subscribe`, `resources/unsubscribe`); and
   list/render prompts (`prompts/list`, `prompts/get`) and completions
   (`completion/complete` for `ref/prompt` and `ref/resource`).
3. **Full MRTR callback brokerage (`roots`, `sampling`, `elicitation`).** Each
   per-server host handle declares only the client capabilities explicitly
   enabled on that handle (`roots`, `sampling`, `elicitation`, `extensions`) and
   dispatches server-originated callbacks — `roots/list` (plus
   `notifications/roots/list_changed`), `sampling/createMessage`, and
   `elicitation/create` (form and URL modes) — across both bidirectional/SSE
   streams and `2026-07-28` mid-request turn-around (`resultType: "input_required"`,
   `requestState` + `inputResponses`). Unadvertised or policy-denied callbacks
   fail closed (`-32601` / `-32021` / explicit operator refusal) without leaking
   host filesystem paths or ambient model credentials.
4. **Official Tasks (`ext-tasks@2026-07-28`) and MCP Apps (`ext-apps@2026-01-26`).**
   Host calls opt into `io.modelcontextprotocol/tasks` per request
   (`call_tool_with_tasks`, `call_tool_resolving_tasks`), persist handles via
   `TaskHandleStore`, poll `tasks/get` respecting `pollIntervalMs` and `ttlMs`,
   route mid-task `inputRequests` through the exact standalone MRTR trust policy
   with lifetime key deduplication via `tasks/update`, cancel via `tasks/cancel`
   with `Mcp-Name`/`Mcp-Method` HTTP headers, and reject unrequested
   `CreateTaskResult` fail-closed. For MCP Apps (`io.modelcontextprotocol/apps`),
   Core negotiates extension capabilities, resolves `ui://` resources, validates
   CSP/sandbox/origin metadata, and preserves `structuredContent` and App resource
   links losslessly in headless Core while interactive Apps viewports live in
   Susi Commercial Apps.
5. **Authorization, transport policy, and credential isolation (`CLAIM-04`).**
   `McpServerConfig::http_scope` and `authorize_transport` enforce `MacPolicy`
   before spawning or dialing (`process.exec` for stdio, `network.egress` for
   non-loopback HTTP, `filesystem.read`/`filesystem.write` for filesystem-scoped
   servers). Plaintext Bearer tokens are restricted to verified loopback targets
   (`ensure_loopback`) or HTTPS / Ed25519-signed + `v1s` ChaCha20-Poly1305
   sealed cluster channels (`susi_core::mcp_client`). `401 WWW-Authenticate` and
   `403 insufficient_scope` challenges surface structured metadata (`ext-auth` /
   OAuth 2.1 resource metadata discovery) and credentials bound to server A are
   never transmitted to server B or echoed in error logs.
6. **Notifications, progress correlation, and cooperative cancellation.** The
   client/host demultiplexes `notifications/progress` (strictly correlated to
   active `_meta.progressToken` leases and ignored after terminal settlement),
   `notifications/message`, `notifications/resources/updated`,
   `notifications/*/list_changed`, and `notifications/tasks`, and emits
   `notifications/cancelled` (`requestId` + optional `reason`) when a caller
   timeout or cancellation token fires (using `tasks/cancel` rather than
   `notifications/cancelled` for durable tasks).
7. **Concurrent multi-server isolation.** A host managing multiple concurrent
   servers isolates per-server `Implementation` client identity, principal tag,
   bearer/signing material, negotiated `ProtocolVersion` and `Mcp-Session-Id`,
   allowed `roots` list, MRTR callback handlers, active subscriptions, task
   handle namespaces, and circuit/cooldown state. Latency spikes, malformed
   frames, or crashes on one server never block, poison, or cross-contaminate
   sibling servers.
8. **Zero vendor SDK crates and real reference-server verification.** All client
   and host types are first-party Rust (`#![forbid(unsafe_code)]` on `susi-gmcp`,
   `susi-wire-client`, and `susi-abi`, `#![deny(unsafe_code)]` on `susi-tools`).
   `susi-vendor-mcp`, `rmcp`, `schemars`, `axum`, `tower`, `reqwest`, and `ureq`
   remain absent from Core manifests, and every `.01`..`.06` + `.M` test exercises
   real shipped GMCP servers.

## Pinned sources

Normative ledger row (`VC-203-017` in `.agents/gmcp/requirements-ledger.json`):
- `CLAIM-04` (`susi@baseline-2026-10-17`, `docs/gmcp-roadmap.md` line `105`,
  sha256 `e816149f5d7c5cbb551ce6a1715ee518a0c902ef66194bc833c4adc3aa2bac34`,
  `owner: "susi-tools"`, `production_path: "crates/susi-tools/src/mcp_budget.rs"`):
  replaces the retired `susi-vendor-mcp` (`rmcp`) facade claim with Susi's
  first-party std/HTTP/stdio MCP client and host stack.

Upstream protocol, client/server primitive, pattern, security, and official-extension
authorities pinned in `.agents/gmcp/sources.json` and vendored under `.agents/gmcp/spec/`:

| repository | document | commit | sha256 |
|---|---|---|---|
| modelcontextprotocol | `schema/2026-07-28/schema.json` | `86f6e0c675afea9a8f233e09b6faf566b126086c` | `ef70b61f99b6d2e5e3b46863822eab08dff6a45bedc7a08914e0e5b133f40203` |
| modelcontextprotocol | `schema/2025-11-25/schema.json` | `86f6e0c675afea9a8f233e09b6faf566b126086c` | `268a5f82ba70fd7e4b6dc4aa1e64f116f74b4d0edcb69dc046829c79dd4e97e7` |
| modelcontextprotocol | `docs/specification/2026-07-28/architecture/index.mdx` | `86f6e0c675afea9a8f233e09b6faf566b126086c` | `9037b342a1f23c056a564a48dda15b70e511dda6d09e96ab0e86f6f9086f9cc3` |
| modelcontextprotocol | `docs/specification/2026-07-28/basic/versioning.mdx` | `86f6e0c675afea9a8f233e09b6faf566b126086c` | `bc02f271700bdecd88034f2d7801eb09110a71d65876487378d94e48dfeb90dc` |
| modelcontextprotocol | `docs/specification/2026-07-28/basic/transports/stdio.mdx` | `86f6e0c675afea9a8f233e09b6faf566b126086c` | `6fd49766c40dc093d1f5993ab584bad0a06cbb496c2d3019c50f8fe3c8171e57` |
| modelcontextprotocol | `docs/specification/2026-07-28/basic/transports/streamable-http.mdx` | `86f6e0c675afea9a8f233e09b6faf566b126086c` | `22574bf11e004068493787203ce92be1162107cad717bcb805a15780d4fa69c9` |
| modelcontextprotocol | `docs/specification/2026-07-28/basic/authorization/index.mdx` | `86f6e0c675afea9a8f233e09b6faf566b126086c` | `99e7eeaecad5381649e6dca98cb5b47f314bdda9f1d8cd989299e5fe143a913e` |
| modelcontextprotocol | `docs/specification/2026-07-28/basic/patterns/mrtr.mdx` | `86f6e0c675afea9a8f233e09b6faf566b126086c` | `a8671eb2a0d292c0dcbae19666ff4fc3d97f294ce8107e9f38259cc5a02a066b` |
| modelcontextprotocol | `docs/specification/2026-07-28/basic/patterns/subscriptions.mdx` | `86f6e0c675afea9a8f233e09b6faf566b126086c` | `8333cbc3280cad293e96b4a20c3200face8e185ebf58f1c4a51c35bb96654abd` |
| modelcontextprotocol | `docs/specification/2026-07-28/basic/patterns/progress.mdx` | `86f6e0c675afea9a8f233e09b6faf566b126086c` | `9f55aca08920c633f3a2a35e0c77533db08d3fd19ced43918ea7c57aa5f85409` |
| modelcontextprotocol | `docs/specification/2026-07-28/basic/patterns/cancellation.mdx` | `86f6e0c675afea9a8f233e09b6faf566b126086c` | `396030784a4af9b734a1c8d4faed13b31077e4a3a1f03c9c1f2048ec52794ba4` |
| modelcontextprotocol | `docs/specification/2026-07-28/client/roots.mdx` | `86f6e0c675afea9a8f233e09b6faf566b126086c` | `016aea34d76ccce86a82509dbf2699833bb8547bde5b3f4a8eb086dfd26a9ba2` |
| modelcontextprotocol | `docs/specification/2026-07-28/client/sampling.mdx` | `86f6e0c675afea9a8f233e09b6faf566b126086c` | `32fe2bb69f5a7caa85a6b9501f465cb1fb888d0c669cd13b23a071071a029df1` |
| modelcontextprotocol | `docs/specification/2026-07-28/client/elicitation.mdx` | `86f6e0c675afea9a8f233e09b6faf566b126086c` | `74351d1081681695536e59a55588a67ed66a76db6057846c263ce2f465fab4c2` |
| modelcontextprotocol | `docs/specification/2026-07-28/server/discover.mdx` | `86f6e0c675afea9a8f233e09b6faf566b126086c` | `3fe1f5b5f1528014216b1e49cc3363b3c689c36bcb80a6957ddca6a04cea409c` |
| modelcontextprotocol | `docs/specification/2026-07-28/server/tools.mdx` | `86f6e0c675afea9a8f233e09b6faf566b126086c` | `ed550806a58eb7744b858fb9f26001aa5e55dac9e2babe88317cc81d9d4c490d` |
| modelcontextprotocol | `docs/specification/2026-07-28/server/resources.mdx` | `86f6e0c675afea9a8f233e09b6faf566b126086c` | `10538119019af5f49a2fbc55d5818725175863115dfb273e79ed7f65a18aaf0c` |
| modelcontextprotocol | `docs/specification/2026-07-28/server/prompts.mdx` | `86f6e0c675afea9a8f233e09b6faf566b126086c` | `85e550635bbbbaa90dff8738f98808c62987e8cfce1d75305b55ca43f3c546de` |
| modelcontextprotocol | `docs/specification/2026-07-28/server/utilities/completion.mdx` | `86f6e0c675afea9a8f233e09b6faf566b126086c` | `1965c563aadb99e141131444caa81ff1860ed82bab314b286c40a0f0ba839f99` |
| ext-tasks | `ext-tasks/specification/2026-07-28/tasks.md` | `899c1bbc869567670093face3591b39c28ce3219` | `5fc22b9b5ff248703927c69855f751a061c2d6170f4dc3923204452f859f91a5` |
| ext-apps | `ext-apps/specification/2026-01-26/apps.mdx` | `82221c0c8ce7661efa6771c9d461511b1650495f` | `ee452a7d1b9b7fb900acfeb4d6932d3963375b0f3f37d196a4b93eb80312af0e` |
| ext-auth | `ext-auth/specification/stable/enterprise-managed-authorization.mdx` | `fb374c7db2b34f18ca9183882e0beecdf661892b` | `df4fe01daec0eac6069e17a293d4ed7e197ab20463038d806f71e63b22fd8986` |

## Canonical data model

### Session, transport, and capability layer (`crates/susi-wire-client/src/mcp.rs`, `crates/susi-core/src/mcp_client.rs`)

- **Protocol revisions & negotiation**:
  - `PROTOCOL_VERSION_2026_07_28 = "2026-07-28"`: stateless per-request metadata
    (`_meta["io.modelcontextprotocol/protocolVersion"]`,
    `_meta["io.modelcontextprotocol/clientCapabilities"]`, HTTP `Mcp-Method` /
    `Mcp-Name`) bootstrapped via `Session::open_2026_07_28` (`server/discover`).
  - `SUPPORTED_VERSIONS = ["2025-11-25", "2025-06-18", "2025-03-26"]`: stateful
    `Session::open` / `Session::open_with_capabilities` issuing `initialize` ->
    `Mcp-Session-Id` + negotiated `MCP-Protocol-Version` ->
    `notifications/initialized` (`202 Accepted`) -> `DELETE /mcp` teardown on
    `Session::close` / `Drop`.
  - Unrecognized server `protocolVersion` values or missing `Mcp-Session-Id` on
    stateful handshakes fail closed (`WireError::Protocol`).
- **Primitive & extension operations**:
  - Discovery & inspection: `protocol_version()`, `server_capabilities()`,
    `server_extensions()`, `supports_server_tasks()`, `server/discover`.
  - Tools: `tools/list` (cursor-paged up to `MAX_TOOLS_LIST_PAGES = 50`),
    `call_tool`, `call_tool_with_tasks`, `call_tool_resolving_tasks`.
  - Resources & subscriptions: `resources/list`, `resources/templates/list`,
    `resources/read`, `subscriptions/listen`, `resources/subscribe`,
    `resources/unsubscribe`, plus `notifications/resources/updated` and
    `notifications/resources/list_changed` delivery.
  - Prompts & completions: `prompts/list`, `prompts/get`, `completion/complete`
    (`ref/prompt` and `ref/resource`).
  - MRTR callbacks: per-session callback policy and handlers for `roots/list`
    (confined `Root { uri: "file://...", name }` sets + `notifications/roots/list_changed`),
    `sampling/createMessage` (model-preference & token-budget gated), and
    `elicitation/create` (schema-validated form responses & URL-mode consent),
    operating identically for live server-to-client requests and `2026-07-28`
    turn-around `inputRequests` (`requestState` + `inputResponses`).
  - Official Tasks (`TASKS_EXTENSION_ID = "io.modelcontextprotocol/tasks"`):
    `TaskStatus`, `TaskHandle`, `TaskSnapshot`, `InputRequestEntry`,
    `PersistedTaskHandle`, `TaskHandleStore`, `get_task`, `update_task`,
    `cancel_task`, `poll_task_until_done`.
  - MCP Apps (`io.modelcontextprotocol/apps`, `crates/susi-gmcp/src/content.rs`):
    negotiation of Apps extension metadata, `ui://` resource inspection, CSP and
    sandbox policy verification, and lossless `CanonicalContentBlock` /
    `structuredContent` preservation in headless Core (interactive UI rendering
    lives in Susi Commercial Apps).
  - Authorization & peer sealing (`crates/susi-core/src/mcp_client.rs`):
    loopback `~/.susi/api_token`, cluster `peer_bearer`, Ed25519 `signed_headers`,
    `SEALED_STREAM = "v1s"` ChaCha20-Poly1305 chunked sealing
    (`seal_stream_record` / `open_stream_records`), and structured `401`/`403`
    auth-challenge reporting (`WWW-Authenticate`, resource metadata URL, required
    scopes).

### Multi-server host, policy, and CLI (`crates/susi-tools/src/`, `src/cli/mcp_cli.rs`)

- **Configuration & Mandate 35 preservation (`crates/susi-tools/src/config.rs`, `leading_mcp.rs`)**:
  - `McpConfig` (`mcp_servers: HashMap<String, McpServerConfig>`, `extra`),
    `McpServerConfig` (`command`, `args`, `env`, `extra`), and `GlobalMcpEntry`
    preserve every unknown field byte-for-byte across read/modify/write cycles.
  - `LeadingMcpManager` (`list`, `status`, `doctor`, `setup`, `enable`,
    `disable`, `configure`, `reset`) never starts or downloads third-party
    software automatically.
- **Policy & multi-server isolation (`crates/susi-tools/src/mcp_budget.rs`, `client.rs`)**:
  - `McpServerConfig::http_scope` canonicalizes HTTP/HTTPS authority + path
    (defaulting `/` to `/mcp`), strips userinfo/query/fragment secrets from
    policy/error strings, and classifies loopback vs remote egress.
  - `authorize_transport` enforces `MacPolicy` (`process.exec`, `network.egress`,
    `filesystem.read`, `filesystem.write`) per server before any process spawn or
    socket connect.
  - Concurrent server handles (`T-CODEXGMCPROADMAP-115`..`119` host surface) keep
    per-server `clientInfo` identity, bearer/OAuth credentials, allowed `roots`,
    MRTR callback handlers, active subscriptions, progress/cancellation tokens,
    and failure cooldowns strictly partitioned by server id.
- **Unified CLI (`crates/susi-cli-defs/src/mcp.rs`, `src/cli/mcp_cli.rs`)**:
  - `susi mcp` exposes one operator CLI backed by the same client/host runtime
    used programmatically (`serve`, `list`, `status`, `doctor`, `setup`,
    `enable`, `disable`, `configure`, `reset`, `ledger`, `tools`, `call`, and the
    host resource/prompt/completion/task/discover subcommands delivered in
    `T-CODEXGMCPROADMAP-115`..`119`).

## Behavior table

| ID | Level | Normative behavior | Production path |
|---|---|---|---|
| HOST-001 | must | Negotiate both `2026-07-28` stateless (`server/discover` + per-request `_meta`) and `2025-11-25`/`2025-06-18`/`2025-03-26` stateful (`initialize` + `Mcp-Session-Id` + `notifications/initialized` + `DELETE`) sessions, plus stdio and direct/in-process transports, failing closed on unsupported protocol versions or missing session headers. | `crates/susi-wire-client/src/mcp.rs` |
| HOST-002 | must | Enforce `CLAIM-04` over Susi's std/HTTP/stdio JSON-RPC client without `susi-vendor-mcp`, gating every server connection through `MacPolicy` (`process.exec` for stdio, `network.egress` for remote HTTP, `filesystem.read`/`filesystem.write` for declared filesystem capability). | `crates/susi-tools/src/mcp_budget.rs` |
| HOST-003 | must | Discover and retain each server's `serverInfo`, `supportedVersions`, `capabilities`, and `capabilities.extensions` per host connection without cross-server capability leakage. | `crates/susi-wire-client/src/mcp.rs` |
| HOST-004 | must | List tools across cursor pages (`MAX_TOOLS_LIST_PAGES = 50`, failing closed on unbounded cursor loops) and invoke `tools/call` preserving `content`, `structuredContent`, and `isError` semantics. | `crates/susi-wire-client/src/mcp.rs` |
| HOST-005 | must | List static resources (`resources/list`) and URI templates (`resources/templates/list`), read text/blob resources (`resources/read`), and manage filtered subscriptions (`subscriptions/listen`, `resources/subscribe`, `resources/unsubscribe`) with `notifications/resources/updated` and `list_changed` delivery. | `crates/susi-wire-client/src/mcp.rs` |
| HOST-006 | must | List and materialize prompts (`prompts/list`, `prompts/get`) and resolve argument completions (`completion/complete` for `ref/prompt` and `ref/resource`) with lossless `CanonicalContentBlock` preservation. | `crates/susi-wire-client/src/mcp.rs` |
| HOST-007 | must | Broker server-to-client MRTR callbacks (`roots/list` + `notifications/roots/list_changed`, `sampling/createMessage`, `elicitation/create` form & URL modes) across live streams and `2026-07-28` turn-around (`requestState` + `inputResponses`), advertising only registered client capabilities. | `crates/susi-wire-client/src/mcp.rs` |
| HOST-008 | must | Execute Official Tasks (`io.modelcontextprotocol/tasks`) end-to-end (`call_tool_with_tasks`, `call_tool_resolving_tasks`, `get_task`, `update_task` with MRTR trust gating and per-task key deduplication, `cancel_task` with `Mcp-Name`/`Mcp-Method` headers, and `TaskHandleStore` durable resume). | `crates/susi-wire-client/src/mcp.rs` |
| HOST-009 | must | Negotiate MCP Apps (`io.modelcontextprotocol/apps`), inspect `ui://` resources and CSP/sandbox metadata, and preserve `structuredContent` and App resource links in headless Core while interactive UI rendering lives in Susi Commercial Apps. | `crates/susi-gmcp/src/content.rs` |
| HOST-010 | must | Isolate authorization and credentials per server: bind loopback `~/.susi/api_token`, OAuth/Bearer tokens, or cluster Ed25519 `signed_headers` + `v1s` ChaCha20-Poly1305 sealed streams to their target endpoint and surface structured `401`/`403` challenges. | `crates/susi-core/src/mcp_client.rs` |
| HOST-011 | must | Deliver bounded `notifications/progress` (correlated to active `_meta.progressToken` only), `notifications/message`, `notifications/*/list_changed`, and `notifications/tasks`, and emit `notifications/cancelled` (`requestId`, `reason`) on caller abort/timeout. | `crates/susi-wire-client/src/mcp.rs` |
| HOST-012 | must | Operate multiple servers concurrently under one host with isolated client identities, `MacPolicy` scopes, credentials, `ProtocolVersion`/`Mcp-Session-Id`, `roots`, MRTR handlers, subscriptions, and cooldowns so one slow or failing server never degrades or contaminates peers. | `crates/susi-tools/src/client.rs` |
| HOST-013 | must | Expose one unified programmatic Rust API and `susi mcp` CLI backed by the same client/host implementation for capability discovery, tools, resources/subscriptions, prompts/completions, MRTR callbacks, tasks, Apps inspection, and diagnostics. | `src/cli/mcp_cli.rs` |
| HOST-014 | must_not | Accept unrequested `CreateTaskResult` responses, execute unadvertised MRTR callbacks, buffer responses past `MAX_RESPONSE_BYTES` (`16 MiB`), send plaintext Bearer tokens to non-loopback HTTP addresses, or leak URL credentials/tokens in error messages. | `crates/susi-wire-client/src/mcp.rs` |
| HOST-015 | must_not | Link or expose third-party MCP SDK, schema, or HTTP framework crates (`susi-vendor-mcp`, `rmcp`, `schemars`, `axum`, `tower`, `reqwest`, `ureq`) across Core boundaries, or introduce `unsafe` code. | `crates/susi-gmcp/src/lib.rs` |
| HOST-016 | must | Preserve unknown `McpConfig`, `McpServerConfig`, and `GlobalMcpEntry` fields across read/write cycles (Mandate 35) and keep `list`/`status`/`doctor` read-only with zero automatic server launches or downloads. | `crates/susi-tools/src/config.rs` |
| HOST-017 | should | Retry transient transport failures on idempotent discovery/read/poll calls (`MAX_IDEMPOTENT_RETRIES = 3`) with bounded backoff and `Last-Event-ID` SSE resumption without re-running non-idempotent `tools/call` mutations. | `crates/susi-wire-client/src/mcp.rs` |
| HOST-018 | must | Prove every host capability, callback surface, and multi-server isolation invariant against real shipped GMCP reference servers across `gmcp_req_203_017_01_contract` through `06_operations` and `gmcp_vc_203_017_mastery`. | `tests/gmcp_requirements.rs` |

## Compatibility and deprecation decisions

1. **Dual-era protocol support (`2026-07-28` + `2025-11-25`..`2025-03-26`)**:
   - The host speaks both stateless `2026-07-28` (`server/discover` + per-request
     `_meta`) and stateful `2025-11-25`, `2025-06-18`, and `2025-03-26`
     (`initialize` + `Mcp-Session-Id`). Pre-`2025-03-26` HTTP+SSE split-endpoint
     transports remain retired unless bridged through an explicit isolated adapter
     (`VC-203-023`).
2. **Permanent retirement of `susi-vendor-mcp` (`CLAIM-04`)**:
   - `susi-vendor-mcp` (`rmcp`) was removed in `9a3810194` (`2026-11-08`). All
     host and client functionality runs on Susi's first-party `susi-wire-client`,
     `susi-core::mcp_client`, `susi-tools`, and `susi-gmcp` crates.
3. **Mandate 35 forward compatibility**:
   - `McpConfig`, `McpServerConfig`, `GlobalMcpEntry`, and `PersistedTaskHandle`
     retain unknown JSON fields (`#[serde(flatten)] extra`) across upgrades.

## Threat model

1. **Confused-deputy & cross-server credential leakage**:
   - A malicious or compromised server A attempts to harvest Bearer tokens,
     cluster peer keys, or `file://` roots belonging to server B, or redirects an
     HTTP request to an external/internal host. Mitigated by per-server credential
     binding, `ensure_loopback` plaintext-bearer refusal, SSRF/egress `MacPolicy`
     checks, and zero automatic cross-origin `Authorization` forwarding.
2. **Unsolicited MRTR & capability-spoofing attacks**:
   - A rogue server invokes `sampling/createMessage`, `elicitation/create`, or
     `roots/list` when the host handle did not advertise that capability, or
     returns `CreateTaskResult` when per-request task capability was omitted, or
     reuses an `inputRequests` key in `tasks/get` to replay operator consent.
     Mitigated by strict capability gating, fail-closed `CreateTaskResult`
     rejection (`HOST-014`), standalone MRTR policy enforcement on every task
     `inputRequests` entry, and monotonic per-task answered-key deduplication.
3. **Framing, pagination, and notification floods**:
   - A hostile server streams unbounded HTTP/SSE bodies, infinite `nextCursor`
     loops on `tools/list` / `resources/list` / `prompts/list`, oversized log/task
     notifications, or stale `notifications/progress` frames. Mitigated by
     `MAX_RESPONSE_BYTES = 16 MiB`, `MAX_TOOLS_LIST_PAGES = 50`, bounded SSE line
     parsers, per-request `DEFAULT_TIMEOUT = 20s`, and active-token progress
     filtering.
4. **Malicious `ui://` Apps payloads & prompt injection**:
   - An untrusted server returns `ui://` HTML/script payloads or untrusted prompt
     messages claiming elevated trust. Mitigated by keeping Core strictly headless
     (never executing untrusted HTML/JS in Core; interactive Apps sandboxes live
     in Susi Commercial Apps) and validating CSP/origin metadata fail-closed.

## SLOs and resource budgets

| Metric / resource | Normative bound | Enforcement point |
|---|---|---|
| Loopback `server/discover` or `initialize` + `tools/call` p99 | `<= 25 ms` (warm loopback reference server) | `crates/susi-wire-client/src/mcp.rs` |
| Default per-request receive timeout | `20 s` (`DEFAULT_TIMEOUT`, configurable via `set_timeout`) | `crates/susi-wire-client/src/mcp.rs` |
| Multi-server parallel discovery wall-clock ceiling | `<= 12 s` bounded deadline across configured servers | `crates/susi-tools/src/client.rs` |
| Max HTTP / SSE response payload per call | `16 MiB` (`MAX_RESPONSE_BYTES = 16_777_216`) | `crates/susi-wire-client/src/mcp.rs` |
| Max cursor pagination pages per list call | `50` pages (`MAX_TOOLS_LIST_PAGES = 50`) | `src/cli/mcp_cli.rs` |
| Max idempotent transport retries | `3` attempts (`MAX_IDEMPOTENT_RETRIES = 3`) | `crates/susi-wire-client/src/mcp.rs` |
| Persisted `TaskHandleStore` file permissions | `0600` on Unix + atomic temp-file rename | `crates/susi-wire-client/src/mcp.rs` |

## Fixtures

- **Positive contract & reference-server fixtures (`gmcp_req_203_017_01_contract`)**:
  1. Dual concurrent loopback `production_reference_server` instances (`server-a`
     on `2026-07-28` stateless `Session::open_2026_07_28` with token `tok-alpha`,
     `server-b` on stateful `2025-11-25` `Session::open_with_capabilities` with
     token `tok-beta`), proving isolated capability discovery, `tools/list`,
     `tools/call` (`summarize_items`), `resources/list` + `resources/read`
     (`memo://welcome`, `memo://items/42`), `prompts/list` + `prompts/get`
     (`draft_brief`), `completion/complete`, and `call_tool_resolving_tasks`
     (`approval_workflow` with MRTR `inputRequests["approve"]` settlement).
  2. `McpConfig` / `McpServerConfig` Mandate 35 unknown-field round-trip and
     `http_scope()` canonicalization across loopback (`127.0.0.1`, `[::1]`,
     `localhost`) and remote HTTPS endpoints with credential-redacted scopes.
- **Negative contract fixtures (`gmcp_req_203_017_01_contract`)**:
  1. Cross-server token presentation (`tok-alpha` sent to `server-b`) rejected
     with `WireError::Http(401, _)`.
  2. Plaintext Bearer connection to non-loopback `198.51.100.10:9093` refused
     locally before opening a socket (`ensure_loopback`).
  3. Unrequested `CreateTaskResult` on `Session::call_tool("approval_workflow", ...)`
     (without per-request task opt-in) rejected fail-closed.
  4. Non-object client capabilities (`json!(["bad"])`) and malformed `McpServerConfig`
     URLs (`http://`, `http://[::zzz]:80/mcp`, embedded whitespace) rejected.

## Evidence locations

- **Contract & acceptance tests**: `docs/gmcp/contract-203-017.md`,
  `tests/gmcp_requirements.rs`, and `tests/gmcp_requirements/vc_203_017.rs`
  (`gmcp_req_203_017_01_contract` through `gmcp_req_203_017_06_operations` and
  `gmcp_vc_203_017_mastery`).
- **Pinned upstream corpus & ledger**: `.agents/gmcp/sources.json`,
  `.agents/gmcp/requirements-ledger.json` (`CLAIM-04`), and `.agents/gmcp/spec/`.
- **Retained append-only evidence & verdict (strictly absent until `.M` Mastery)**:
  `.agents/gmcp/evidence/VC-203-017.jsonl` and
  `.agents/roadmap-verdicts/VC-203-017.json` **MUST NOT** exist during stages
  `.01` through `.06` (`T-CODEXGMCPROADMAP-114`..`119`) and are minted only in
  `T-CODEXGMCPROADMAP-120` (`[GMCP 203.017.M] Mastery`) once all stage `01..06`
  checks and `gmcp_vc_203_017_mastery` pass on `HEAD`.

## Ambiguities resolved

1. **Headless Core host vs UI rendering (`ext-apps`)**: Core implements the full
   protocol-level MCP Apps host contract (extension negotiation, `ui://` resource
   fetch, CSP/sandbox validation, and `structuredContent` fidelity), while
   interactive graphical rendering surfaces live in Susi Commercial Apps.
2. **Per-server MRTR & root scope**: Client capabilities (`roots`, `sampling`,
   `elicitation`) and `file://` root allow-lists are configured per server handle,
   never shared globally across unrelated servers.
3. **Stateless vs stateful MRTR**: Both `2026-07-28` mid-request turn-around
   (`resultType: "input_required"`, `requestState` + `inputResponses`, and
   `tasks/get` + `tasks/update`) and bidirectional stream server-to-client
   JSON-RPC requests (`roots/list`, `sampling/createMessage`,
   `elicitation/create`) pass through the identical host callback policy gate.
4. **Cancellation routing**: In-flight JSON-RPC requests cancel via
   `notifications/cancelled` (`params.requestId`), whereas durable Official Tasks
   cancel exclusively via `tasks/cancel` (`params.taskId` + HTTP `Mcp-Name`).

## Operations and non-claims

- **Rollout & per-server isolation boundary**: `McpHost` isolates transport sessions, `HostPolicy` client identities/principals, `file://` root allow-lists, MRTR callback handlers, active SSE subscriptions, notification ring buffers, and `PersistedTaskHandleStore` files per server handle. Disconnecting, disabling, or resetting one server never interrupts sibling connected servers or mutates caller transcripts.
- **Headless Core vs. Commercial UI boundary**: Susi Core ships the complete protocol-level MCP host, session engine, callback broker, headless `ui://` MCP Apps metadata/CSP validator, and `susi mcp` CLI with zero vendor MCP SDK crates (`rmcp`, `schemars`, `axum`, `tower`, `reqwest`, `ureq` absent), while interactive WebView/graphical MCP Apps surfaces live in Susi Commercial Apps (`intellibitz/susi-commercial-apps`).

## Operations and incident runbook

### Inspectable state

- `McpHost::snapshot()` emits a deterministic, credential-free JSON inspection document (`serverCount`, `limits.{maxBufferedNotifications,maxInterleavedFrames,maxPaginationPages}`, and `servers[*].{serverId,transport,endpoint,sessionId,protocolVersion,clientIdentity,advertisedCapabilities,allowedRootsCount,persistedTaskCount,serverInfo,serverCapabilities,subscribedResources,bufferedNotifications}`). Neither `McpHost::snapshot()` nor `Debug` formatting ever includes Bearer tokens or URL credentials.
- `susi mcp status` and `susi mcp list` emit inspectable JSON control-plane status for configured and leading MCP servers (`LeadingMcpManager::status()`), while `susi mcp doctor [<server>]` audits launcher and environment readiness without starting third-party processes.
- `susi mcp discover <addr> [--legacy] [--token <tok>]`, `susi mcp tools <addr>`, `susi mcp call <addr> <tool> [<args_json>]`, `susi mcp resources <addr> [--templates]`, `susi mcp read <addr> <uri> [--app]`, `susi mcp prompts <addr>`, `susi mcp prompt <addr> <name> [<args_json>]`, `susi mcp complete <addr> <prompt|resource> <target> <arg> <value>`, and `susi mcp task <addr> <get|update|cancel> <task_id> [<responses_json>]` inspect and operate live peer HTTP and configured stdio MCP servers over the exact production `Session` / `McpHost` substrate.
- `susi mcp ledger` prints the canonical JSON verification report matching the MCP `ledger_verify` tool verbatim, and `GmcpService::ops_snapshot()` exposes embedded/local server catalog and plugin state.

### Configuration validation

- CLI flag validation fails closed when `--refetched <dir>` is passed without `--drift` on `susi mcp ledger` (`clap` `requires = "drift"`).
- Host startup validation (`susi_config::validate::validate_dir`) audits `~/.susi/config.json` and reports malformed `max_rpc_body_bytes` values with `ConfigIssueKind::TypeMismatch`.
- `McpServerConfig::http_scope` canonicalizes HTTP/HTTPS server endpoints, strips embedded `user:secret@` userinfo, `?query`, and `#fragment` secrets from policy scope strings, and fails closed on empty hosts, invalid IPv6 literals, or embedded whitespace.
- `authorize_mcp_transport` and `spawn_authorized_stdio_peer` enforce `MacPolicy` (`process.exec` for stdio child processes, `network.egress` for remote HTTP endpoints) prior to process spawn or socket dial, and `Session` (`ensure_loopback`) refuses plaintext Bearer tokens toward non-loopback IP addresses before opening a TCP connection.
- `RootEntry::new` rejects non-`file://` schemes and `..` path traversal segments; `parse_app_resource_summary` fails closed on non-`ui://` URIs, non-object `_meta.ui.csp` or `_meta.ui.permissions`, and invalid `_meta.ui.domain` hostnames; `PersistedTaskHandleStore::open_or_quarantine` quarantines corrupt JSON files atomically to `.corrupt` and enforces `0600` file permissions on Unix.

### Bounded metrics, logs and traces

- Transport, framing, pagination, notification, and retry bounds prevent resource exhaustion across every connected server:
  - HTTP / SSE response ceiling: `MAX_RESPONSE_BYTES = 16_777_216` (`16 MiB`) and stdio duplex line ceiling `16 MiB`.
  - Cursor pagination ceiling: `MAX_PAGINATION_PAGES = 50` (`MAX_TOOLS_LIST_PAGES = 50`), plus duplicate-cursor loop rejection.
  - Per-server notification & interleaving ceilings: `MAX_BUFFERED_NOTIFICATIONS = 256` and `MAX_INTERLEAVED_FRAMES = 1024`.
  - Default request deadline & retry ceilings: `DEFAULT_TIMEOUT = 20s` and `MAX_IDEMPOTENT_RETRIES = 3`.
- On Streamable HTTP `/mcp`, W3C `traceparent` headers and `_meta["io.modelcontextprotocol/logLevel"]` emit bounded `notifications/message` SSE frames when `logLevel` is requested, strip Bearer tokens so secrets never appear in SSE/log payloads or error strings, and suppress `notifications/message` when `logLevel` is omitted.

### Diagnostics and incident triage

- `WireError` typed host/client diagnostics: `WireError::Io` (socket/EOF/timeout failure), `WireError::Http(status, body)` (HTTP `401`/`403`/`413`/5xx), `WireError::Rpc(code, message)` (JSON-RPC `-32600`, `-32601`, `-32602`, `-32020`, `-32021`, `-32022`, `-32603`), and `WireError::Protocol(reason)` (version mismatch, unrequested `CreateTaskResult`, duplicate callback ID, CSP/root violation, cursor loop, or frame bound breach).
- `probe_http_auth_challenge` and `parse_www_authenticate` extract structured OAuth 2.1 / `ext-auth` challenge fields (`scheme`, `realm`, `resource_metadata`, `scope`, `error`, `error_description`) on `401`/`403` responses without echoing caller Bearer tokens.
- `hash_drift` / `unfetchable`: deterministic findings emitted by `susi mcp ledger --drift --refetched <dir>` whenever any of the 22 pinned `VC-203-017` upstream specification, schema, client/server primitive, or extension documents changes or goes missing relative to `.agents/gmcp/sources.json`.

### Examples

```text
# 1. Inspect host control-plane status, ledger parity, and peer server capabilities
susi mcp ledger
susi mcp status
susi mcp discover 127.0.0.1:9093 --token "$SUSI_API_TOKEN"
susi mcp tools 127.0.0.1:9093 --token "$SUSI_API_TOKEN"
susi mcp call 127.0.0.1:9093 summarize_items '{"items":["a","b"],"scope":{"namespace":"ops"}}' --token "$SUSI_API_TOKEN"

# 2. Inspect resources, headless MCP Apps metadata, prompts, completions, and Official Tasks
susi mcp resources 127.0.0.1:9093 --templates --token "$SUSI_API_TOKEN"
susi mcp read 127.0.0.1:9093 ui://app/summary --app --token "$SUSI_API_TOKEN"
susi mcp prompts 127.0.0.1:9093 --token "$SUSI_API_TOKEN"
susi mcp prompt 127.0.0.1:9093 draft_brief '{"topic":"security"}' --token "$SUSI_API_TOKEN"
susi mcp complete 127.0.0.1:9093 prompt draft_brief topic se --token "$SUSI_API_TOKEN"
susi mcp task 127.0.0.1:9093 get "$TASK_ID" --token "$SUSI_API_TOKEN"

# 3. Run deterministic air-gapped standards drift check and VC-203-017 operations gate
susi mcp ledger --drift --refetched .agents/gmcp/spec
cargo test --locked --test gmcp_requirements gmcp_req_203_017_06_operations
```

### Incident triage

Treat any `verified: false` standards drift (`hash_drift` or `unfetchable`), cross-server credential or root leak, unrequested `CreateTaskResult` acceptance, unredacted Bearer token in `McpHost::snapshot()` or SSE `notifications/message`, `MacPolicy` bypass on stdio/HTTP egress, or sibling server hang during peer disconnect/flood as a release-blocking incident. Capture `susi mcp ledger --drift` JSON output, `susi mcp status`, `McpHost::snapshot()`, quarantined `.corrupt` task-store artifacts, and the latest `.agents/gmcp/evidence/*.jsonl` records before remediation, then re-verify `gmcp_req_203_017_01_contract` through `gmcp_req_203_017_06_operations`.

### Migration

1. **Migrating `~/.susi/mcp_config.json` entries**: Register HTTP or stdio servers via `GmcpClient::admit_mcp_server` or `susi mcp enable <server>`; `McpConfig` and `McpServerConfig` preserve all unknown vendor fields (`#[serde(flatten)] extra`) per Mandate 35.
2. **Upgrading stateful `2025-03-26`..`2025-11-25` servers to `2026-07-28`**: Switch peer connections from `McpHost::connect_http_session` (`susi mcp discover --legacy`) to `McpHost::connect_http_2026_07_28` (`susi mcp discover`) while keeping `HostPolicy` callbacks, `RootEntry` sets, and `PersistedTaskHandleStore` paths unchanged.
3. **Upstream specification re-pinning**: Vendor updated spec files under `.agents/gmcp/spec/`, update SHA-256 digests in `.agents/gmcp/sources.json`, update the 22-row table in `docs/gmcp/contract-203-017.md`, re-run `gmcp_req_203_017_01_contract`..`06_operations`, and refresh two-phase GMCP evidence (`scripts/gmcp-record-evidence.sh` + `--mastery`).

### Rollback

1. **Per-server live disconnect & config rollback**: Invoke `McpHost::disconnect(server_id)` (issuing `DELETE /mcp` for stateful HTTP sessions or terminating stdio child peers) and `susi mcp disable <server>` / `susi mcp reset <server>` to remove a misbehaving peer immediately while sibling connected servers remain online.
2. **Task handle store recovery**: If a persisted task store is corrupted on disk, `PersistedTaskHandleStore::open_or_quarantine` renames the damaged file to `.corrupt` for forensics and initializes a clean `0600` store without crashing the host.
3. **Repository/spec rollback**: Revert the offending commit via `git revert <sha>`, verify `susi_config::validate::validate_dir` and `susi mcp ledger --drift --refetched .agents/gmcp/spec` (`verified: true`), re-run `gmcp_req_203_017_06_operations`, and append fresh GMCP evidence records without truncating `.agents/gmcp/evidence/*.jsonl`.

### Production guarantees and non-claims

- Production guarantees cover `crates/susi-wire-client/src/{host,mcp}.rs`, `crates/susi-tools/src/{mcp_budget,client,config,leading_mcp}.rs`, `crates/susi-core/src/mcp_client.rs`, `src/cli/mcp_cli.rs`, and `crates/susi-cli-defs/src/mcp.rs` (`HOST-001`..`HOST-018`).
- Non-claim: Susi Core validates MCP Apps (`io.modelcontextprotocol/apps`) negotiation, `ui://` resource payloads, CSP, permissions, and domain metadata headlessly; sandboxed browser/WebView rendering surfaces live exclusively in `intellibitz/susi-commercial-apps`.
- Non-claim: Direct Unix-domain socket host/server transport checks compile under `#[cfg(unix)]`, while TCP loopback Streamable HTTP and stdio child/duplex transports run across both Linux and Windows x86_64 hosts.
