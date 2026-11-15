# Contract GMCP-203-020-01 — GMCP server authoring SDK and plugin lifecycle

Status: **implemented** (requirements GMCP-203-020-01..02, vector VC-203-020)
Baseline date: **2026-11-15**

## Normative clause

A stable Susi-shaped authoring SDK builds local MCP servers, tools, resources,
prompts, subscriptions, MRTR handlers, tasks and extensions with schema
derivation, typed context, middleware, testing fixtures and hot reload. Plugins
declare permissions and compatibility, initialize transactionally, unload
without leaks, preserve no vendor types across the boundary, and ship runnable
minimal through production examples.

Per `docs/gmcp-roadmap.md` (`VC-203-020`) and `crates/susi-config/src/extensions.rs`
(zero-UI plan A9), Core owns the protocol-generic GMCP server authoring SDK and
the fail-closed plugin/extension-pack lifecycle substrate (`~/.susi/extensions/<id>/`),
while bundling and seeding **zero** curated vendor plugin packs in Core (curated
plugin packs live in Susi Commercial Apps).

The contract binds eight normative obligations across `VC-203-020`:

1. **Unified Susi-shaped server authoring surface.** Local MCP servers are
   assembled through an explicit builder/service surface (`ServerBuilder` over
   `GmcpService`) with explicit `Implementation` identity, deterministic
   `server/discover` metadata (`DISCOVER_TTL_MS = 0`, `DISCOVER_CACHE_SCOPE =
   CacheScope::Private`), and zero implicit ambient global state. Every MCP
   server primitive — tools, static resources, RFC 6570 resource URI templates,
   resource access guards, prompts, `completion/complete` providers, filtered
   subscriptions, server-to-client MRTR calls (elicitation, `sampling/createMessage`,
   `roots/list`), durable tasks, and namespaced extensions — registers on one
   coherent catalog and dispatches identically across in-process embedded,
   stdio, Unix socket (`#[cfg(unix)]`), and Streamable HTTP `/mcp` entry points.
2. **Compile-time schema derivation and fail-closed local compilation.** Tool
   input and output schemas derive deterministically from Susi-owned Rust shapes
   (`SchemaType`, `StructSchema`, `schema::root`, `input_schema!`, `tool_def!`)
   emitting JSON Schema `https://json-schema.org/draft/2020-12/schema` with
   `$defs`/`$ref` composition, `Option<T>` nullability/optionality,
   `#[serde(default)]` defaults, `#[serde(rename = "...")]` wire naming, and doc
   comment descriptions. Registration compiles every `inputSchema` and optional
   `outputSchema` locally via `compile_schema` (`schema_check::Validator`),
   refusing remote/file `$ref` targets, cycles, unknown types, and complexity
   bombs fail-closed before catalog admission.
3. **Typed execution contexts and composable middleware.** Handlers receive
   Susi-owned typed contexts (`RequestContext<RoleServer>`,
   `NotificationContext<RoleServer>`, `SubscriptionContext`, `TaskContext`)
   exposing authenticated principal identity, negotiated `ProtocolVersion`,
   peer `ClientCapabilities`, cancellation tokens, progress leases, and MRTR
   client request helpers. Composable middleware wraps request and tool
   execution in deterministic outer-to-inner order (policy, rate/budget checks,
   structured audit, redaction) without bypassing `jsonrpc_gate` framing or
   compiled `inputSchema`/`outputSchema` validation.
4. **Deterministic testing fixtures and hot reload.** Authoring fixtures provide
   an in-process loopback/duplex MCP test harness (`TestHarness` / duplex
   transport + isolated scratch directories) that exercises full JSON-RPC wire
   negotiation, tool/resource/prompt calls, subscriptions, MRTR, tasks, and
   plugin reload without opening OS network sockets or mutating the operator's
   `~/.susi`. Hot reload compiles and validates replacement catalog snapshots
   off-line, swaps the active catalog atomically, emits `notifications/*/list_changed`
   (and `notifications/resources/updated` for replaced URIs), and lets in-flight
   requests and tasks complete against the snapshot under which they started.
5. **Plugin permission and compatibility declarations.** Plugins and extension
   packs (`~/.susi/extensions/<id>/manifest.json`) declare `id`, `name`,
   `version`, `api_version` (checked against `HOST_PACK_API_MAJOR = 1`),
   `capabilities`, `requires`, `optional`, `permissions` (validated against
   `KNOWN_PERMISSIONS = ["filesystem.read", "filesystem.write", "network.egress",
   "process.exec"]`), confined relative `files`, SHA-256 `checksums`, and
   optional Ed25519 `signatures` + `signer`. Unknown permissions, incompatible
   `api_version` majors, missing required host capabilities, unpermitted `..`
   path escapes, digest mismatches, and invalid/untrusted signatures fail closed.
6. **Transactional initialization and leak-free unload.** Attaching or loading a
   plugin/pack executes in a stage-verify-commit transaction: manifest
   validation, SHA-256/Ed25519 verification, required capability checks, and
   handler/schema compilation all complete before any catalog entry or
   `state.json` transition becomes visible. Any failure rolls back atomically,
   preserving the previous active catalog and subscriptions untouched. Unloading
   a plugin detaches every contributed tool, resource, template, prompt,
   completion, and extension, cancels or drains owned watchers/subscriptions/tasks,
   drops open descriptors, and restores fallback defaults (`DEFAULT_PACK_ID =
   "default"`) with zero leaked handles or stale advertised capabilities.
7. **Zero vendor types across the boundary.** Public authoring traits, builder
   types, wire envelopes, schema descriptors, content blocks
   (`CanonicalContentBlock`), plugin manifests (`ExtensionManifest`), and IPC
   frames (`susi-abi`) are 100% Susi-owned Rust types (`#![forbid(unsafe_code)]`).
   Third-party MCP SDK or schema-generation crates (`rmcp`, `schemars`, `axum`,
   `tower`) are never linked or re-exported across crate boundaries; the legacy
   module alias `susi_gmcp::rmcp` aliases Susi's own internal `susi_gmcp::mcp`
   module solely for internal source compatibility.
8. **Runnable minimal-through-production examples.** The SDK ships runnable,
   test-verified examples spanning a minimal single-tool server through a full
   production server combining derived schemas, resources + guards, prompts +
   completions, filtered subscriptions, MRTR elicitation/sampling, durable
   tasks, namespaced extensions, middleware, atomic hot reload, and
   transactional plugin load/unload.

Baseline schema derivation (`crates/susi-gmcp/src/mcp/schema.rs`), core handler
registration (`crates/susi-gmcp/src/protocol.rs`), durable task execution
(`crates/susi-gmcp/src/mcp/task_manager.rs`), extension governance
(`crates/susi-gmcp/src/extensions.rs`), zero-vendor crate boundaries
(`crates/susi-gmcp/src/lib.rs`, `crates/susi-abi/src/lib.rs`), and manifest
validation (`crates/susi-config/src/extensions.rs`) exist today; unified
`ServerBuilder`, composable middleware, in-memory authoring `TestHarness`,
transactional catalog/plugin hot reload & attach/detach rollback, and the
minimal-through-production example suite are normative obligations delivered and
verified across `T-CODEXGMCPROADMAP-108` through `T-CODEXGMCPROADMAP-113`.

## Pinned sources

Protocol, server-primitive, security, and official-extension authorities are
pinned in `.agents/gmcp/sources.json` and vendored under `.agents/gmcp/spec/`:

| repository | document | commit | sha256 |
|---|---|---|---|
| modelcontextprotocol | `schema/2026-07-28/schema.json` | `86f6e0c675afea9a8f233e09b6faf566b126086c` | `ef70b61f99b6d2e5e3b46863822eab08dff6a45bedc7a08914e0e5b133f40203` |
| modelcontextprotocol | `schema/2026-07-28/schema.ts` | `86f6e0c675afea9a8f233e09b6faf566b126086c` | `742750af0bb8c716e7030c4977c992b55d1adc4407e9e66997db5846baedc2cd` |
| modelcontextprotocol | `schema/2025-11-25/schema.json` | `86f6e0c675afea9a8f233e09b6faf566b126086c` | `268a5f82ba70fd7e4b6dc4aa1e64f116f74b4d0edcb69dc046829c79dd4e97e7` |
| modelcontextprotocol | `docs/specification/2026-07-28/server/tools.mdx` | `86f6e0c675afea9a8f233e09b6faf566b126086c` | `ed550806a58eb7744b858fb9f26001aa5e55dac9e2babe88317cc81d9d4c490d` |
| modelcontextprotocol | `docs/specification/2026-07-28/server/resources.mdx` | `86f6e0c675afea9a8f233e09b6faf566b126086c` | `10538119019af5f49a2fbc55d5818725175863115dfb273e79ed7f65a18aaf0c` |
| modelcontextprotocol | `docs/specification/2026-07-28/server/prompts.mdx` | `86f6e0c675afea9a8f233e09b6faf566b126086c` | `85e550635bbbbaa90dff8738f98808c62987e8cfce1d75305b55ca43f3c546de` |
| modelcontextprotocol | `docs/specification/2026-07-28/server/discover.mdx` | `86f6e0c675afea9a8f233e09b6faf566b126086c` | `3fe1f5b5f1528014216b1e49cc3363b3c689c36bcb80a6957ddca6a04cea409c` |
| modelcontextprotocol | `docs/specification/2026-07-28/server/utilities/completion.mdx` | `86f6e0c675afea9a8f233e09b6faf566b126086c` | `1965c563aadb99e141131444caa81ff1860ed82bab314b286c40a0f0ba839f99` |
| modelcontextprotocol | `docs/specification/2026-07-28/basic/patterns/subscriptions.mdx` | `86f6e0c675afea9a8f233e09b6faf566b126086c` | `8333cbc3280cad293e96b4a20c3200face8e185ebf58f1c4a51c35bb96654abd` |
| modelcontextprotocol | `docs/specification/2026-07-28/basic/patterns/mrtr.mdx` | `86f6e0c675afea9a8f233e09b6faf566b126086c` | `a8671eb2a0d292c0dcbae19666ff4fc3d97f294ce8107e9f38259cc5a02a066b` |
| modelcontextprotocol | `docs/docs/2026-07-28/tutorials/security/local-server-security.mdx` | `86f6e0c675afea9a8f233e09b6faf566b126086c` | `fc32d31ea13da8e87d1875eeea2ff656ca66625cbbf10e13cdfff76fddd01ecd` |
| ext-tasks | `ext-tasks/specification/2026-07-28/tasks.md` | `899c1bbc869567670093face3591b39c28ce3219` | `5fc22b9b5ff248703927c69855f751a061c2d6170f4dc3923204452f859f91a5` |
| ext-apps | `ext-apps/specification/2026-01-26/apps.mdx` | `82221c0c8ce7661efa6771c9d461511b1650495f` | `ee452a7d1b9b7fb900acfeb4d6932d3963375b0f3f37d196a4b93eb80312af0e` |
| ext-auth | `ext-auth/specification/stable/enterprise-managed-authorization.mdx` | `fb374c7db2b34f18ca9183882e0beecdf661892b` | `df4fe01daec0eac6069e17a293d4ed7e197ab20463038d806f71e63b22fd8986` |

## Canonical data model

### Authoring SDK surface (`susi_gmcp`)

- **Server construction and catalog (`crates/susi-gmcp/src/protocol.rs`, `crates/susi-gmcp/src/sdk.rs` / `protocol.rs` seam)**:
  - `GmcpService` + `ServerBuilder`: explicit `workspace: PathBuf`, optional durable task store (`open_durable`), `Implementation` identity (`name`, `version`, optional `title`/`description`), `Catalog` (`tools`, `resources`, `templates`, `prompts`, `completions`, `resource_guard`), `TaskManager`, and `ExtensionRegistry`.
  - Registration APIs: `register_tool` (`Tool` + `ToolHandler`), `register_task_tool` (`Tool` + `TaskToolHandler`), `register_resource` (`Resource` + `ResourceHandler`), `register_template` (`ResourceTemplate` + `ResourceHandler`), `set_resource_guard` (`ResourceGuard`), `register_prompt` (`Prompt` + `PromptHandler`), `register_completion` (`CompletionHandler`), `resource_updated`, and `extensions()`.
- **Schema derivation (`crates/susi-gmcp/src/mcp/schema.rs`, `crates/susi-gmcp/src/schema_check.rs`)**:
  - `SchemaType`, `StructSchema`, `Field`, `schema::root<T: StructSchema>()`, `schema::reference<T>()`, `schema::description()`, `input_schema!`, and `tool_def!`.
  - Emits JSON Schema `2020-12` objects compiled locally via `compile_schema` / `Validator::compile` (`MAX_DEPTH = 64`, `MAX_NODES = 10_000`, local `#/$defs/*` references only).
- **Typed contexts (`crates/susi-gmcp/src/mcp/service.rs`, `crates/susi-gmcp/src/mcp/task_manager.rs`)**:
  - `RequestContext<RoleServer>`: principal identity, negotiated `ProtocolVersion`, `ClientCapabilities`, request `_meta`, `CancellationToken`, `progress_lease()`, and MRTR server-to-client calls (`create_message`, `elicit`, `list_roots`).
  - `NotificationContext<RoleServer>` & `SubscriptionContext`: bounded notification emission (`MAX_NOTIFICATION_PAYLOAD_BYTES`, `MAX_LOG_NOTIFICATIONS_PER_REQUEST`, `MAX_ACTIVE_SUBSCRIPTIONS_PER_SESSION`).
  - `TaskContext`: principal-bound task lifecycle (`report_progress`, `request_input` composing MRTR inside tasks, cancellation check, terminal output validation).
- **Middleware, fixtures, and hot reload (`T-CODEXGMCPROADMAP-108`..`112` contract target)**:
  - `Middleware` (`ToolMiddleware` / `RequestMiddleware`): ordered `Arc<dyn Fn(...)>` chain executing around handler dispatch after `jsonrpc_gate` and `inputSchema` validation and before `outputSchema` seal, with access to typed request metadata, principal, and structured error mapping.
  - `TestHarness`: deterministic in-process client-server fixture pairing `GmcpService` with an embedded duplex session and isolated temporary workspace/state root (`0` OS TCP binds required for unit/contract fixtures).
  - `CatalogSnapshot` / `reload_catalog`: transactional swap of tool/resource/prompt/completion/extension registrations; computes deltas and emits `notifications/tools/list_changed`, `notifications/resources/list_changed`, `notifications/prompts/list_changed`, and `notifications/resources/updated` only upon commit.

### Plugin and extension-pack lifecycle (`susi_config::extensions` & `susi_gmcp`)

- **Manifest & state (`crates/susi-config/src/extensions.rs`)**:
  - `ExtensionManifest`: `id`, `name`, `version`, `api_version` (`HOST_PACK_API_MAJOR = 1`), `description`, `capabilities`, `requires`, `optional`, `permissions` (`KNOWN_PERMISSIONS`), `files`, `checksums` (SHA-256), `signatures` (Ed25519), `signer` (32-byte hex public key), and `extra` (Mandate 35 round-trip map).
  - `ExtensionsState` (`~/.susi/extensions/state.json`): `active` (`DEFAULT_PACK_ID = "default"` when none active), `loaded`, `unloaded`, `trusted_digests` (TOFU / explicit re-admission pinning), `trusted_signers`, `quarantined`, and `extra`.
- **Transactional lifecycle (`PluginRegistration` / pack attach & detach)**:
  - **Stage & validate**: `validate_manifest`, SHA-256 + Ed25519 verification, `requires` capability check against host capability set, permission check against `KNOWN_PERMISSIONS`, and `compile_schema` check for every contributed tool/prompt/resource/extension.
  - **Commit**: Atomically publish the new catalog entries and persist `state.json` via temp-file + atomic rename.
  - **Rollback**: On any validation, signature, capability, or initializer error, discard staged registrations, record quarantine reason when integrity fails, and leave the prior catalog, active pack, and live subscriptions byte-for-byte unchanged.
  - **Unload**: Remove only the unloaded plugin's owned catalog keys and extension ids, cancel/drain plugin-owned subscriptions/tasks/watchers, drop file/channel handles, and emit `list_changed` notifications so peers immediately observe the narrowed surface.
- **Boundary purity (`crates/susi-gmcp/src/lib.rs`, `crates/susi-gmcp/src/content.rs`, `crates/susi-abi/src/lib.rs`)**:
  - `CanonicalContentBlock`, `susi_gmcp::mcp::model::*`, `ExtensionManifest`, and `susi_abi::WireFrame` expose zero third-party MCP SDK or JSON-schema crate types across public signatures.

## Behavior table

Rows are normative (`18` clauses: `17` `must`, `1` `must_not`). Each row binds
its RFC requirement level, normative clause, and owning repository path.

| id | level | clause | production path |
|---|---|---|---|
| SDK-001 | must | Local MCP servers assemble via an explicit Susi-shaped builder/service API over `GmcpService` with explicit `Implementation` identity and zero implicit ambient global state. | crates/susi-gmcp/src/protocol.rs |
| SDK-002 | must | `input_schema!`, `tool_def!`, `SchemaType`, `StructSchema`, and `schema::root` derive JSON Schema 2020-12 (`$schema`, `$defs`/`$ref`, `Option<T>`, `#[serde(default)]`, `#[serde(rename)]`, doc descriptions) compiled locally without network or file `$ref` fetches. | crates/susi-gmcp/src/mcp/schema.rs |
| SDK-003 | must | Registered tools validate inbound arguments against compiled `inputSchema` prior to handler execution and validate structured outputs against compiled `outputSchema` prior to wire serialization. | crates/susi-gmcp/src/protocol.rs |
| SDK-004 | must | Static resources and RFC 6570 URI templates register typed handlers gated prior to read or subscribe by `ResourceGuard`, enforcing `MAX_RESOURCE_BYTES` (`8 MiB`) fail-closed without truncation. | crates/susi-gmcp/src/protocol.rs |
| SDK-005 | must | Prompts and `completion/complete` handlers register with deterministic argument definitions, bounded completion results, and `notifications/prompts/list_changed` emission on mutation. | crates/susi-gmcp/src/protocol.rs |
| SDK-006 | must | Authoring surfaces support modern filtered subscriptions (`SubscriptionFilter` for resources, list changes, and `taskIds`) and legacy session subscriptions within `MAX_ACTIVE_SUBSCRIPTIONS_PER_SESSION`. | crates/susi-gmcp/src/mcp/service.rs |
| SDK-007 | must | Tool and task handlers invoke server-to-client MRTR operations (elicitation, `sampling/createMessage`, `roots/list`) through typed context handles and fail closed when peer capabilities are absent or cancelled. | crates/susi-gmcp/src/mcp/service.rs |
| SDK-008 | must | Long-running work registers via `register_task_tool` / `TaskManager` with principal ownership, progress leases, MRTR composition, `open_durable` restart reconciliation, and terminal-state immutability. | crates/susi-gmcp/src/mcp/task_manager.rs |
| SDK-009 | must | Namespaced extensions register, negotiate, version, isolate, and disable through `ExtensionRegistry` without forking core protocol dispatch, refusing key collisions and unsupported required versions. | crates/susi-gmcp/src/extensions.rs |
| SDK-010 | must | Handlers receive typed `RequestContext<RoleServer>`, `NotificationContext<RoleServer>`, `SubscriptionContext`, and `TaskContext` carrying principal, negotiated `ProtocolVersion`, capabilities, and cancellation. | crates/susi-gmcp/src/mcp/service.rs |
| SDK-011 | must | Composable middleware wraps request and tool handlers in deterministic outer-to-inner order for policy, budget, redaction, and audit hooks without bypassing `jsonrpc_gate` or schema validation. | crates/susi-gmcp/src/protocol.rs |
| SDK-012 | must | Deterministic testing fixtures execute full client-server MCP round-trips in-process over loopback/duplex streams and isolated scratch state without opening OS sockets or touching `~/.susi`. | crates/susi-gmcp/src/mcp/transport.rs |
| SDK-013 | must | Hot reload validates replacement catalog snapshots off-line, swaps registrations atomically, emits `list_changed` notifications, and preserves snapshot isolation for in-flight requests and tasks. | crates/susi-gmcp/src/protocol.rs |
| SDK-014 | must | Plugin manifests declare `api_version` (`HOST_PACK_API_MAJOR = 1`), capabilities, `requires`/`optional` dependencies, `KNOWN_PERMISSIONS`, SHA-256 `checksums`, and Ed25519 `signatures`/`signer`, failing closed on unknown permissions, major mismatch, or unpermitted path escapes. | crates/susi-config/src/extensions.rs |
| SDK-015 | must | Plugin initialization and pack load execute transactionally: failed validation, digest/signature mismatch, missing required host capability, or failed handler/schema init rolls back completely with zero partial state. | crates/susi-config/src/extensions.rs |
| SDK-016 | must | Plugin unload removes all plugin-contributed catalog entries and extensions, cancels or drains owned subscriptions/watchers/tasks, releases handles without leaks, and restores compiled fallback defaults. | crates/susi-config/src/extensions.rs |
| SDK-017 | must_not | Public SDK, `CanonicalContentBlock`, `ExtensionManifest`, and `susi-abi` boundaries must not expose third-party vendor MCP/schema types (`rmcp`, `schemars`, `axum`, `tower`), and Core must not bundle or seed vendor plugin packs. | crates/susi-gmcp/src/lib.rs |
| SDK-018 | must | Runnable minimal-through-production server examples and positive/negative lifecycle fixtures execute end-to-end against the shipped GMCP runtime. | tests/gmcp_requirements.rs |

## Compatibility and deprecation decisions

1. **Dual-era wire compatibility from one authoring definition.** Authors write
   handlers once against Susi-shaped SDK types. The runtime serves primary
   stateless/sessionless `2026-07-28` (`_meta["io.modelcontextprotocol/protocolVersion"]`,
   `server/discover`, `subscriptions/subscribe`, task handles) and explicit
   stateful `2025-11-25` (`initialize` -> `notifications/initialized`,
   `resources/subscribe`) through one `GmcpService` catalog without duplicate
   author code.
2. **Host plugin API major rule (`HOST_PACK_API_MAJOR = 1`).** `api_version`
   follows major-version compatibility (`"1"`, `"1.0"`, `"1.x"` match major `1`;
   `"0.x"` or `"2.x"` fail closed at `validate_manifest`). Optional host
   capabilities (`optional`) degrade gracefully with structured status visibility;
   missing `requires` capabilities refuse load transactionally.
3. **Unknown manifest/state fields preserved (Mandate 35).** Both
   `ExtensionManifest.extra` and `ExtensionsState.extra` preserve forward-compatible
   unknown JSON keys verbatim across parse -> write-back while still enforcing
   strict validation on all security-critical fields (`id`, `api_version`,
   `permissions`, `files`, `checksums`, `signatures`, `signer`).
4. **Zero bundled plugins in Core.** Core ships `DEFAULT_PACK_ID = "default"`
   with empty vendor catalogs and seeds zero packs into `~/.susi/extensions/`.
   Curated department and ecosystem plugin packs ship exclusively in Susi
   Commercial Apps (`intellibitz/susi-commercial-apps`) consuming this contract.
5. **Internal `rmcp` alias deprecation posture.** `pub use self::mcp as rmcp;`
   in `crates/susi-gmcp/src/lib.rs` aliases Susi's own `#![forbid(unsafe_code)]`
   first-party `mcp` module; external crates and examples author against
   `susi_gmcp` / `susi_gmcp::mcp` / `susi_gmcp::sdk` types, and no external
   `rmcp` crate exists in `Cargo.toml`.

## Threat model

1. **Privilege escalation via unknown or undeclared permissions.** A malicious
   plugin declares `"permissions": ["shell.root"]` or omits `"filesystem.read"`
   while mapping `"files": {"cfg": "../secrets.json"}`. Mitigation:
   `validate_manifest` rejects any string outside `KNOWN_PERMISSIONS` and
   rejects any `path_escapes_pack_root` mapping unless `"filesystem.read"` is
   explicitly declared; absolute file paths and Windows reserved device names
   (`CON`, `NUL`, `COM1`, etc.) are unconditionally rejected.
2. **Tampered or unsigned plugin artifacts and silent signer rotation.** An
   attacker modifies a managed file on disk or swaps `signer` in `manifest.json`
   after initial admission. Mitigation: `load_pack` verifies SHA-256 `checksums`
   and 64-byte Ed25519 `signatures` over raw bytes, pins `trusted_digests` and
   `trusted_signers` in `state.json` on first admission (TOFU), quarantines
   mismatched packs in `quarantined`, and refuses signer rotation without
   explicit re-admission.
3. **Partial-initialization state corruption.** A plugin registers two tools and
   then fails while compiling a third tool's schema or checking a `requires`
   capability, leaving half-initialized tools callable. Mitigation: staged
   transactional load compiles every schema and validates every capability into
   an isolated staging set before atomic commit; any error discards the entire
   staging set with zero mutation to `Catalog` or `state.json`.
4. **Hot-reload race conditions and torn catalog reads.** Concurrent `tools/call`
   or `tools/list` traffic arrives mid-reload. Mitigation: replacement catalogs
   are validated off-line and swapped under a single write critical section (or
   `Arc` snapshot swap); in-flight handlers clone their `Arc` handler/validator
   reference before awaiting, preventing torn reads, deadlocks, or use-after-unload.
5. **Post-unload capability, subscription, or handle leaks.** Unloading a plugin
   leaves stale tools in `tools/list`, orphaned `SubscriptionFilter` streams
   receiving events, or background tasks keeping pack files locked. Mitigation:
   plugin-owned registrations carry provenance tags (`plugin_id`), and unload
   atomically purges all owned tools/resources/templates/prompts/completions/extensions,
   cancels/drains associated subscriptions and tasks, and asserts post-unload
   catalog parity with the pre-load baseline.
6. **Schema complexity bombs and SSRF via `$ref`.** A plugin or server author
   supplies a deeply nested JSON Schema, cyclic `$defs`, or remote
   `"$ref": "https://..."`. Mitigation: `compile_schema` enforces local-only
   `#/$defs/*` resolution, cycle detection, `MAX_DEPTH = 64`, and
   `MAX_NODES = 10_000` before admitting any tool.
7. **Middleware bypass of protocol security gates.** Custom middleware attempts
   to accept malformed JSON-RPC envelopes, skip `inputSchema` checks, or emit
   unvalidated `outputSchema` payloads. Mitigation: `jsonrpc_gate` and
   `compile_schema` input validation run unconditionally before user middleware,
   and `enforce_output_schema` + `CanonicalContentBlock` boundary checks seal the
   response before wire egress.
8. **Cross-principal task or session leakage in SDK contexts.** A handler or
   plugin attempts to read or cancel another principal's task or reuse session
   state across tenants. Mitigation: `RequestContext::principal()` scopes
   `TaskManager` ownership (`try_spawn_for`, `get_for`, `cancel_for`) and
   `GmcpService::session()` isolates per-connection state.
9. **Host environment pollution during fixture tests.** SDK tests accidentally
   write to the developer's real `~/.susi` or collide on fixed TCP ports.
   Mitigation: `TestHarness` uses in-memory duplex streams and unique temporary
   roots (`isolate_home`).
10. **Vendor type coupling across crate boundaries.** Exposing third-party crate
    types in public signatures locks callers to upstream breaking changes and
    unsafe transitive code. Mitigation: `susi-gmcp`, `susi-config`, and
    `susi-abi` enforce `#![forbid(unsafe_code)]` and zero third-party SDK types
    in public signatures, verified continuously by `lean_core_ceilings` and
    `gmcp_requirements`.

## SLOs and resource budgets

| resource / operation | budget / ceiling | enforcement point |
|---|---|---|
| Schema compilation & content tree depth | `MAX_DEPTH = 64`, `MAX_NODES = 10_000` | `crates/susi-gmcp/src/schema_check.rs`, `crates/susi-gmcp/src/content.rs` |
| Single resource read payload | `MAX_RESOURCE_BYTES = 8 MiB` (`8_388_608` bytes; refused, never truncated) | `crates/susi-gmcp/src/protocol.rs` |
| Active subscriptions per session / filter | `MAX_ACTIVE_SUBSCRIPTIONS_PER_SESSION`, `MAX_RESOURCE_SUBSCRIPTIONS_PER_FILTER` | `crates/susi-gmcp/src/mcp/service.rs` |
| Notification payload & per-request logs | `MAX_NOTIFICATION_PAYLOAD_BYTES`, `MAX_LOG_NOTIFICATIONS_PER_REQUEST` | `crates/susi-gmcp/src/mcp/service.rs` |
| Task lifecycle budgets | `DEFAULT_TASK_TTL_MS = 300_000`, `MAX_ACTIVE_TASKS_PER_PRINCIPAL = 64`, `MAX_TOOL_CALL_SECS = 1_800` | `crates/susi-gmcp/src/mcp/task_manager.rs`, `crates/susi-gmcp/src/server.rs` |
| Wire framing bounds | Stdio `DEFAULT_MAX_LINE_LENGTH = 16 MiB`, HTTP `DEFAULT_MAX_REQUEST_BODY_BYTES = 4 MiB`, `max_rpc_body_bytes > 0` | `crates/susi-gmcp/src/stdio.rs`, `crates/susi-gmcp/src/mcp/http.rs` |
| Atomic catalog hot-reload swap | `< 5 ms` p99 critical-section swap for `<= 256` tools/resources/prompts; `0` dropped in-flight calls | `crates/susi-gmcp/src/protocol.rs` |
| Transactional plugin load / unload | `< 50 ms` p99 in-process pack validate + attach/detach (excluding cold OS disk flush); `0` residual catalog entries, subscriptions, tasks, or open file descriptors post-unload | `crates/susi-config/src/extensions.rs`, `crates/susi-gmcp/src/protocol.rs` |
| Middleware dispatch overhead | `< 100 us` p99 per 4-stage in-memory middleware chain | `crates/susi-gmcp/src/protocol.rs` |

## Fixtures

Positive and negative contract fixtures live in `tests/gmcp_requirements/vc_203_020.rs`
(included into `tests/gmcp_requirements.rs`) and execute under
`gmcp_req_203_020_01_contract`:

- **Positive fixtures**:
  1. Nested `input_schema!` derivation (`FilterSpec` + `QueryArgs`) exercising
     `$schema` (`2020-12`), `$defs`/`$ref`, `Option<T>` nullability,
     `#[serde(default)]`, `#[serde(rename = "...")]`, doc-comment descriptions,
     and `GmcpService::register_tool` compilation.
  2. Valid `ExtensionManifest` fixture (`api_version = "1"`, `KNOWN_PERMISSIONS`,
     confined relative `files`, valid SHA-256 `checksums`, preserved Mandate 35
     `extra` fields) passing `susi_config::extensions::validate_manifest`.
  3. Workspace dependency boundary check proving `crates/susi-gmcp/Cargo.toml`
     and `crates/susi-abi/Cargo.toml` expose zero external `rmcp`, `schemars`,
     `axum`, or `tower` crates.
- **Negative fixtures**:
  1. Remote/unresolvable `$ref` schema (`"$ref": "https://evil.example/s.json"`)
     rejected fail-closed by `GmcpService::register_tool` without mutating the
     registered tool catalog.
  2. Plugin manifest declaring unknown permission (`"kernel.root"`) rejected by
     `validate_manifest`.
  3. Plugin manifest declaring incompatible major `api_version` (`"2.0"`)
     rejected against `HOST_PACK_API_MAJOR = 1`.
  4. Plugin manifest escaping pack root (`"../escape.json"`) without
     `"filesystem.read"` rejected fail-closed.
  5. Plugin manifest declaring `signatures` without `signer` and malformed
     SHA-256 digest rejected fail-closed.

Subsequent stages (`T-CODEXGMCPROADMAP-108`..`113`) extend `vc_203_020.rs` with
middleware ordering, in-memory `TestHarness`, concurrent hot-reload,
transactional plugin rollback, post-unload zero-leak verification, transport
parity, CLI/API operations diagnostics, and runnable minimal-through-production
examples (`01_minimal_tool` through `06_production_plugin_server`).

## Evidence locations

- **Contract & acceptance suite**: `docs/gmcp/contract-203-020.md`,
  `tests/gmcp_requirements.rs`, and `tests/gmcp_requirements/vc_203_020.rs`
  (`gmcp_req_203_020_01_contract` through `gmcp_req_203_020_06_operations` and
  `gmcp_vc_203_020_mastery`).
- **Pinned upstream corpus**: `.agents/gmcp/sources.json` and
  `.agents/gmcp/spec/**`, verified via
  `susi_gmcp::requirements_ledger::verify_pinned_ledger`.
- **Append-only vector evidence policy**: `.agents/gmcp/evidence/VC-203-020.jsonl`
  remains **absent** during `T-CODEXGMCPROADMAP-107`..`112` and is created and
  sealed in `T-CODEXGMCPROADMAP-113` (`[GMCP 203.020.M] Mastery`) once all six
  stage gates (`01`..`06`) and `gmcp_vc_203_020_mastery` pass on the shipped
  runtime. Because `docs/gmcp` and `tests/gmcp_requirements*` participate in the
  shared GMCP artifact tree digest (`xtask/src/gmcp_evidence.rs`), every commit
  series in `T-CODEXGMCPROADMAP-107`..`112` refreshes existing delivered vector
  evidence files via two-phase `cargo local-release record-gmcp` +
  `cargo local-release record-gmcp --mastery`.

## Ambiguities resolved

- **SDK vs. bundled plugins boundary.** `VC-203-020` delivers the complete
  protocol-generic server authoring SDK and plugin host lifecycle in Core while
  shipping **zero** bundled vendor plugin packs in Core (`DEFAULT_PACK_ID =
  "default"`). Curated commercial plugin packs live in Susi Commercial Apps, and
  runnable minimal-through-production SDK/plugin examples live as self-contained
  first-party examples/fixtures that never auto-seed `~/.susi/extensions/`.
- **Meaning of "preserve no vendor types across the boundary".** Every public
  type, trait, error, context, schema builder, content block, and plugin
  manifest exposed by `susi-gmcp`, `susi-config`, and `susi-abi` is defined by
  Susi workspace crates (using only standard library primitives and Susi's
  `susi-vendor-serde` / `susi-vendor-serde-json` / `susi-vendor-tokio` facades
  where async/JSON values are part of the Rust signature). No third-party MCP
  SDK (`rmcp`), schema generator (`schemars`), HTTP framework (`axum`, `tower`),
  or plugin runtime (`wasmtime` host types) ever crosses the public GMCP SDK or
  plugin manifest boundary.
- **Transactional initialization semantics.** Plugin load/attach is all-or-nothing:
  if a plugin contributes `N` tools/resources/prompts/extensions and item `K`
  fails validation, schema compilation, capability negotiation, or init hook
  execution, `0` of the `N` items appear in `Catalog` or `ExtensionRegistry`,
  and `ExtensionsState.active` retains its prior value.
- **Hot-reload vs. in-flight execution.** Hot reload swaps the catalog for
  subsequent requests immediately upon atomic commit and emits `list_changed`
  notifications, while any tool call, resource read, MRTR exchange, or durable
  task already admitted before the swap finishes deterministically under its
  captured handler and output validator (unless explicitly cancelled).
- **Leak-free unload definition.** After unloading plugin `P`, (a) `Catalog` and
  `ExtensionRegistry` contain zero entries attributed to `P`, (b) active
  subscriptions and watchers owned by `P` are closed (`0` retained senders),
  (c) tasks spawned under `P`'s handlers are cancelled or drained before unload
  completes, and (d) reloading `P` with an identical manifest succeeds idempotently
  without collision errors (`ExtensionError::Collision`) or growing baseline
  counts.
- **Middleware ordering.** Wire framing (`BoundedLines` / HTTP body limits),
  `jsonrpc_gate` protocol validation, and compiled `inputSchema` checks execute
  *before* middleware; registered middlewares execute outer-to-inner around the
  handler; compiled `outputSchema` validation and `CanonicalContentBlock`
  sanitization execute *after* handler/middleware completion before wire egress.
