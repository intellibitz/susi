# GMCP-203-012 contract: Subscriptions, notifications, progress, and operational messages

This contract binds every normative clause of vector VC-203-012. Each row in the behavior table is normative; its identifier, level, clause, and production path are pinned against `.agents/gmcp/requirements-ledger.json` and `.agents/gmcp/sources.json`.

## Normative clause

The shipped GMCP runtime in `crates/susi-gmcp/src/protocol.rs` and its supporting `crate::mcp` transport/service boundary must implement all 57 pinned normative clauses of VC-203-012 (`26` modern `2026-07-28`, `15` optional, and `16` compatibility `2025-11-25` adapter clauses):

1. **Modern subscription streams (`subscriptions/listen`)**: Clients open long-lived notification streams carrying an explicit `SubscriptionFilter` (`toolsListChanged`, `promptsListChanged`, `resourcesListChanged`, `resourceSubscriptions`). The server MUST intersect the requested filter with server capabilities and resource authorization policy (`admit_resource` / `ResourceGuard`), emit `notifications/subscriptions/acknowledged` with `_meta["io.modelcontextprotocol/subscriptionId"]` matching the request `id` **before** any notification on that subscription, tag every delivered notification with `_meta["io.modelcontextprotocol/subscriptionId"]`, NEVER emit unrequested or denied notification types, bound subscriber fan-out and queues, re-check resource authorization before emitting `notifications/resources/updated`, end cleanly on `notifications/cancelled` (stdio) or stream close (HTTP) or graceful shutdown (`resultType: "complete"`), and treat reconnects as stateless (`subscriptions/listen` re-issued by the client).
2. **Request-scoped progress (`notifications/progress`)**: When a request includes a valid string or integer `_meta.progressToken` unique across active requests, the server MAY emit monotonically increasing `progress` updates (rate-limited, with optional `total` and redacted human-readable `message`), MUST correlate progress tokens strictly to active requests or non-terminal tasks, and MUST stop emitting progress upon completion, error, cancellation, or task termination.
3. **Operational log messages (`notifications/message`, deprecated SEP-2577)**: In `2026-07-28`, operational notifications are deprecated in favor of `stderr` (stdio) or OpenTelemetry; when supported on compatibility/requested flows, the server declares the `logging` capability, obeys RFC 5424 severity ordering via per-request `_meta["io.modelcontextprotocol/logLevel"]`, rejects malformed log levels (`-32602`), NEVER emits `notifications/message` when `io.modelcontextprotocol/logLevel` is absent, NEVER delivers `notifications/message` on a `subscriptions/listen` stream, enforces credential/PII/secret redaction plus rate and payload bounds so logging and progress cannot act as an unbounded covert channel, and restricts legacy `logging/setLevel` plus `resources/subscribe`/`resources/unsubscribe` strictly to the `2025-11-25` compatibility adapter.

## Pinned sources

Upstream authority: `modelcontextprotocol` revision `86f6e0c675afea9a8f233e09b6faf566b126086c` (`2026-10-15T00:54:40Z`), with modern revision `2026-07-28` and compatibility adapter baseline `2025-11-25`. All SHA-256 digests match `.agents/gmcp/sources.json` and `.agents/gmcp/spec/`.

| repository | document | sha256 | lines |
|---|---|---|---|
| modelcontextprotocol | docs/specification/2025-11-25/basic/utilities/progress.mdx | 35d27ca9ad8ee9e029ab84afc751de208f7732c97c7a5d792abbcc237ab72528 | 16-94 |
| modelcontextprotocol | docs/specification/2025-11-25/server/utilities/logging.mdx | 37cfde22e75d2444c9d796c2df636b96c1c9d486e64b109e38169f2d7f2cf82a | 19-136 |
| modelcontextprotocol | docs/specification/2026-07-28/basic/patterns/index.mdx | 9e08dd8b295cacc59bd4e233e9e1d6bfcc149695c265d95a18e2fddc9ad3968d | 20-26 |
| modelcontextprotocol | docs/specification/2026-07-28/basic/patterns/progress.mdx | 9f55aca08920c633f3a2a35e0c77533db08d3fd19ced43918ea7c57aa5f85409 | 8-90 |
| modelcontextprotocol | docs/specification/2026-07-28/basic/patterns/subscriptions.mdx | 8333cbc3280cad293e96b4a20c3200face8e185ebf58f1c4a51c35bb96654abd | 15-161 |
| modelcontextprotocol | docs/specification/2026-07-28/server/utilities/logging.mdx | 4e77ec3257e07ad69bfd6ed902771f14d6498b60fe6c9c325b8ff4bfc35647a6 | 13-128 |

## Canonical data model

### Shipped surfaces (verified baseline in `crates/susi-gmcp/src/`)

- **`GmcpService` (`crates/susi-gmcp/src/protocol.rs`)**:
  - `changes: broadcast::Sender<Change>` (`Change::Tools`, `Change::Resources`, `Change::Prompts`, `Change::Resource(String)`), bounded capacity `256`.
  - `legacy_subscriptions: Arc<RwLock<BTreeSet<String>>>` and `log_level: Arc<RwLock<Option<LoggingLevel>>>` isolated per `GmcpService::session()`.
  - `GmcpService::subscribe` / `GmcpService::unsubscribe` / `GmcpService::set_level` / `GmcpService::on_initialized` (legacy compatibility path; `subscribe` calls `admit_resource(&request.uri)` and returns `RESOURCE_NOT_FOUND` on unknown resources).
  - `GmcpService::accepted_subscription_filter` / `GmcpService::listen` (modern `subscriptions/listen` handler using `SubscriptionContext` and `SubscriptionSink`).
  - `PROGRESS_GAP` (`100ms`, max ~10 Hz) and `relay_progress` coalescing watched tool progress between `0.0` (`"Tool started"`) and `1.0` (`"Tool finished"`, skipped when `context.ct.is_cancelled()`) using `sent / (sent + 1.0)`.
- **Wire models (`crates/susi-gmcp/src/mcp/model.rs`)**:
  - `SubscriptionFilter` (`toolsListChanged`, `promptsListChanged`, `resourcesListChanged`, `resourceSubscriptions`) with `intersection(&self, other)` and `supported_by(&self, capabilities)`.
  - `SubscriptionsListenRequestParams { notifications: SubscriptionFilter }`.
  - `ProgressNotificationParam { progress_token, progress, total, message, meta }` and `CancelledNotificationParam { request_id, reason }`.
  - `LoggingLevel` (`Debug`, `Info`, `Notice`, `Warning`, `Error`, `Critical`, `Alert`, `Emergency`), `SetLevelRequestParams`, `LoggingMessageNotificationParam { level, logger, data, meta }`.
  - Metadata constants: `META_LOG_LEVEL = "io.modelcontextprotocol/logLevel"`, `META_SUBSCRIPTION_ID = "io.modelcontextprotocol/subscriptionId"`; `RequestMetaObject` helpers `get_progress_token()`, `log_level()`.
- **Service & framing boundaries (`crates/susi-gmcp/src/mcp/service.rs`, `jsonrpc_gate.rs`, `mcp/transport.rs`, `mcp/http.rs`)**:
  - `SubscriptionContext::establish` emits `notifications/subscriptions/acknowledged` with accepted `SubscriptionFilter` and `_meta["io.modelcontextprotocol/subscriptionId"]` before constructing `SubscriptionSink`.
  - `SubscriptionSink::notify_*` checks cancellation (`SubscriptionClosed`), accepted filter (`NotificationNotAccepted`), sets `META_SUBSCRIPTION_ID`, and forwards via `Peer::send_notification` (`mpsc::channel(1024)`).
  - `handle_request` gates `subscriptions/listen` to modern sessions (`-32601` on legacy) and `resources/subscribe` / `resources/unsubscribe` to legacy sessions (`-32601` on modern); graceful `listen()` return serializes `{"resultType": "complete", "_meta": {"io.modelcontextprotocol/subscriptionId": id, "io.modelcontextprotocol/serverInfo": ...}}`.
  - `serve_inner` binds outbound response `sink_tx` capacity `64` and per-request `CancellationToken` map `in_flight` triggered by `notifications/cancelled`.
  - `jsonrpc_gate.rs` and `mcp/transport.rs` allowlist `subscriptions/listen`, `resources/subscribe`, `resources/unsubscribe`, `logging/setLevel`, and the standard `notifications/{subscriptions/acknowledged, tools/list_changed, prompts/list_changed, resources/list_changed, resources/updated, progress, message, cancelled}` methods; `mcp/transport.rs` enforces `DEFAULT_MAX_LINE_LENGTH = 16 MiB` and `mcp/http.rs` enforces `DEFAULT_MAX_REQUEST_BODY_BYTES = 4 MiB`.

### Contract-mandated additions for `T-CODEXGMCPROADMAP-94`..`98` (target production-path invariants, not yet implemented)

- **Authorization-aware subscription admission and live re-check (`protocol.rs`)**: `accepted_subscription_filter` must filter `resourceSubscriptions` through `admit_resource` (removing unauthorized or unknown URIs before acknowledging, never admitting denied paths), and `listen` must re-run `admit_resource` before dispatching every `Change::Resource(uri)` notification so runtime policy/guard changes immediately suppress unauthorized updates.
- **Bounded subscription fan-out & active progress registry (`protocol.rs`, `mcp/service.rs`)**: Enforce an explicit concurrent subscription cap per session and server, cap `resourceSubscriptions` URI count per `SubscriptionFilter`, track active `progressToken` values to reject duplicate active tokens and suppress progress emissions after completion/error/cancellation/terminal task states.
- **Strict modern logging & covert-channel defense (`mcp/model.rs`, `mcp/service.rs`, `protocol.rs`)**: Preserve raw `_meta` presence checks so unrecognized `io.modelcontextprotocol/logLevel` values fail with JSON-RPC `-32602` (`Invalid params`) rather than silently defaulting to `None`; gate `logging/setLevel` to legacy (`2025-11-25` adapter) sessions (`-32601` on modern); implement total RFC 5424 severity ordering (`debug <= info <= notice <= warning <= error <= critical <= alert <= emergency`); never emit `notifications/message` on a `subscriptions/listen` stream; redact credentials/secrets, PII, and internal path/host details in `LoggingMessageNotificationParam.data`, `logger`, and `ProgressNotificationParam.message` using shared `susi_config::redact_credentials` / `susi_core::untrusted_content::wrap_tool_output(...).redacted_for_sink()`, with per-request message count and payload-byte ceilings.
- **Streamable HTTP SSE parity (`mcp/http.rs`, `mcp/message.rs`)**: Route `subscriptions/listen` acknowledgments, tagged notifications, cancellation, and graceful completion cleanly on both stdio and Streamable HTTP SSE streams without cross-session leakage.

## Behavior table

| id | level | clause | production path |
|---|---|---|---|
| MCP-2025-1125-basic-utilities-progress-mdx-0016 | must | - Progress tokens **MUST** be a string or integer value | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-progress-mdx-0017 | must | - Progress tokens can be chosen by the sender using any means, but **MUST** be unique | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-progress-mdx-0033 | may | The receiver **MAY** then send progress notifications containing: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-progress-mdx-0053 | must | - The `progress` value **MUST** increase with each notification, even if the total is | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-progress-mdx-0055 | may | - The `progress` and the `total` values **MAY** be floating point. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-progress-mdx-0056 | should | - The `message` field **SHOULD** provide relevant human readable progress information. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-progress-mdx-0060 | must | 1. Progress notifications **MUST** only reference tokens that: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-progress-mdx-0064 | may | 2. Receivers of progress requests **MAY**: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-progress-mdx-0069 | must | 3. For [task-augmented requests](./tasks), the `progressToken` provided in the original request **MUST** continue to be used for progress notifications throughout the task's lifetime, even after the `CreateTaskResult` has been returned. The progress token remains valid and associated with the task until the task reaches a terminal status. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-progress-mdx-0070 | must | - Progress notifications for tasks **MUST** use the same `progressToken` that was provided in the initial task-augmented request | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-progress-mdx-0071 | must | - Progress notifications for tasks **MUST** stop after the task reaches a terminal status (`completed`, `failed`, or `cancelled`) | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-progress-mdx-0092 | should | - Senders and receivers **SHOULD** track active progress tokens | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-progress-mdx-0093 | should | - Both parties **SHOULD** implement rate limiting to prevent flooding | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-progress-mdx-0094 | must | - Progress notifications **MUST** stop after completion | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-server-utilities-logging-mdx-0019 | must | Servers that emit log message notifications **MUST** declare the `logging` capability: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-server-utilities-logging-mdx-0049 | may | To configure the minimum log level, clients **MAY** send a `logging/setLevel` request: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-server-utilities-logging-mdx-0110 | should | Servers **SHOULD** return standard JSON-RPC errors for common failure cases: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-server-utilities-logging-mdx-0117 | should | 1. Servers **SHOULD**: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-server-utilities-logging-mdx-0123 | may | 2. Clients **MAY**: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-server-utilities-logging-mdx-0131 | must_not | 1. Log messages **MUST NOT** contain: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-server-utilities-logging-mdx-0136 | should | 2. Implementations **SHOULD**: | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-index-mdx-0020 | must_not | Servers **MUST NOT** initiate JSON-RPC requests, and clients do not send | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-index-mdx-0026 | may | While the request is in flight, the server **MAY** send notifications scoped | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-progress-mdx-0008 | may | operations through notification messages. The server **MAY** send progress notifications | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-progress-mdx-0016 | must | - Progress tokens **MUST** be a string or integer value | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-progress-mdx-0017 | must | - Progress tokens can be chosen by the client using any means, but **MUST** be unique | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-progress-mdx-0033 | may | The server **MAY** then send progress notifications containing: | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-progress-mdx-0053 | must | - The `progress` value **MUST** increase with each notification, even if the total is | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-progress-mdx-0055 | may | - The `progress` and the `total` values **MAY** be floating point. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-progress-mdx-0056 | should | - The `message` field **SHOULD** provide relevant human readable progress information. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-progress-mdx-0060 | must | 1. Progress notifications **MUST** only reference tokens that: | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-progress-mdx-0064 | may | 2. Servers receiving a request with a progress token **MAY**: | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-progress-mdx-0088 | should | - Clients and servers **SHOULD** track active progress tokens | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-progress-mdx-0089 | should | - Both parties **SHOULD** implement rate limiting to prevent flooding | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-progress-mdx-0090 | must | - Progress notifications **MUST** stop after completion | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-subscriptions-mdx-0015 | must_not | specifying which event types it wants to receive. The server **MUST NOT** send | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-subscriptions-mdx-0054 | must | The server **MUST** send `notifications/subscriptions/acknowledged` as the first message | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-subscriptions-mdx-0056 | must_not | and **MUST NOT** send any notification on the | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-subscriptions-mdx-0059 | may | subscriptions **MAY** be interleaved before it. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-subscriptions-mdx-0080 | should | The client **SHOULD** check the acknowledged filter against what it requested and handle | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-subscriptions-mdx-0091 | must | share a single channel, clients **MUST** use this field to correlate notifications | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-subscriptions-mdx-0109 | may | A client **MAY** have multiple active subscriptions concurrently — for example, | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-subscriptions-mdx-0122 | should | - The **server** tears it down (e.g., during shutdown) — it **SHOULD** send a | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-subscriptions-mdx-0131 | should | shutdown), it **SHOULD** respond to the original `subscriptions/listen` request | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-subscriptions-mdx-0158 | may | client **MAY** treat as a trigger to reconnect. | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-basic-patterns-subscriptions-mdx-0161 | must | client **MUST** re-send `subscriptions/listen` to re-establish its | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-utilities-logging-mdx-0013 | should_not | before it becomes eligible for removal. New implementations **SHOULD NOT** | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-utilities-logging-mdx-0014 | should | adopt it; existing implementations **SHOULD** migrate to logging to `stderr` | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-utilities-logging-mdx-0032 | must | Servers that emit log message notifications **MUST** declare the `logging` capability: | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-utilities-logging-mdx-0063 | must_not | `io.modelcontextprotocol/logLevel` in the request's `_meta`. The server **MUST NOT** | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-utilities-logging-mdx-0066 | may | When the field is present, the server **MAY** send `notifications/message` | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-utilities-logging-mdx-0069 | must_not | the server **MUST NOT** deliver it on a | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-utilities-logging-mdx-0101 | should | is not a recognized [log level](#log-levels), the server **SHOULD** reject that | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-utilities-logging-mdx-0109 | should | 1. Servers **SHOULD**: | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-utilities-logging-mdx-0115 | may | 2. Clients **MAY**: | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-utilities-logging-mdx-0123 | must_not | 1. Log messages **MUST NOT** contain: | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-server-utilities-logging-mdx-0128 | should | 2. Implementations **SHOULD**: | crates/susi-gmcp/src/protocol.rs |

## Compatibility and deprecation decisions

Ledger disposition tally for `VC-203-012` (`57` rows total): `modern=26`, `optional=15`, `adapter=16`.

- **Legacy `resources/subscribe` & `resources/unsubscribe` (`2025-11-25` adapter)**: Retained exclusively behind the legacy session path (`service.rs` returns `-32601` Method not found on modern `2026-07-28` requests); modern clients MUST use `subscriptions/listen` with `SubscriptionFilter`.
- **Legacy `logging/setLevel` (`2025-11-25` adapter) vs modern per-request `io.modelcontextprotocol/logLevel`**: `logging/setLevel` mutates session-scoped `log_level` strictly on `2025-11-25` adapter sessions (`-94` gates modern requests away from `logging/setLevel` with `-32601`). Modern `2026-07-28` requests control per-request verbosity solely via `_meta["io.modelcontextprotocol/logLevel"]`.
- **SEP-2577 deprecation of MCP operational logging (`2026-07-28`)**: The `logging` / `notifications/message` protocol feature is formally deprecated in `2026-07-28` (minimum 12-month retention window from `2026-07-28` before removal). New Susi surfaces SHOULD NOT emit operational telemetry via MCP `notifications/message` and SHOULD emit operational diagnostics to `stderr` (stdio) or OpenTelemetry; `GmcpService` retains `logging` capability only for explicitly requested compatibility while enforcing redaction, strict level filtering, and `-32602` validation.
- **Task-augmented progress continuity (`2025-11-25` baseline + `TaskManager`)**: Requests promoted to tasks MUST retain the initiating `progressToken` correlation until the task reaches `Completed`, `Failed`, or `Cancelled`, then immediately halt progress notifications.

## Threat model

| threat | attack / failure vector | contract mitigation |
|---|---|---|
| T1: Unbounded fan-out & memory exhaustion | Client opens excessive concurrent `subscriptions/listen` streams, subscribes to thousands of URIs, or stops reading SSE/stdio while catalog mutates | Bound `broadcast::Sender<Change>` (`256`), `Peer` outbound channel (`1024`), response `sink_tx` (`64`), max concurrent subscriptions/session, and max `resourceSubscriptions` entries; terminate lagged slow subscribers immediately rather than growing queues |
| T2: Stale authorization on resource updates | Client subscribes to resource `uri`, then `ResourceGuard`/MAC policy changes to deny `uri` while stream remains open | Filter `resourceSubscriptions` through `admit_resource` before `notifications/subscriptions/acknowledged` AND re-run `admit_resource` on every `Change::Resource(uri)` event prior to notification emission |
| T3: Cross-subscription / cross-session mux confusion | Multiplexed stdio notifications lack or spoof `io.modelcontextprotocol/subscriptionId`, leaking another request's or session's events | Server ALWAYS stamps `META_SUBSCRIPTION_ID` from the originating verified `RequestId` on `acknowledged`, every notification, and graceful completion; isolate `GmcpService::session()` state; `notifications/message` forbidden on `subscriptions/listen` streams |
| T4: Covert exfiltration & secret/PII leakage via logs/progress | Untrusted tool output injects secrets, tokens, PII, or high-frequency bulk payloads into `notifications/message` `data` or `notifications/progress` `message` | Enforce mandatory shared redaction (`susi_config::redact_credentials` / `wrap_tool_output(...).redacted_for_sink()`), omit `notifications/message` unless `_meta["io.modelcontextprotocol/logLevel"]` is explicitly requested, enforce RFC 5424 severity floor, cap message size and emission rate (`PROGRESS_GAP = 100ms` + log rate/byte cap) |
| T5: Spoofed, duplicate, or post-terminal `progressToken` | Client reuses an active `progressToken` across concurrent requests or server emits progress after cancellation / response / terminal task state | Track active `progressToken` set per session to reject active duplicates, cancel `relay_progress` cleanly and gate final `1.0` notify on `!context.ct.is_cancelled()`, and halt task progress upon terminal status |
| T6: Malformed metadata downgrade / validation bypass | Client passes invalid `_meta["io.modelcontextprotocol/logLevel"]` string or attempts `logging/setLevel` / `resources/subscribe` on a `2026-07-28` session | Distinguish absent vs malformed `_meta["io.modelcontextprotocol/logLevel"]` returning `-32602` Invalid params; reject legacy-only RPCs on modern sessions with `-32601` Method not found |

## SLOs and resource budgets

| metric / bound | pinned value | enforcement point & status |
|---|---|---|
| Broadcast catalog event buffer | `256` events (`RecvError::Lagged` closes slow subscriber) | `crates/susi-gmcp/src/protocol.rs` (shipped) |
| Per-session outbound notification queue | `1024` messages (`mpsc::channel(1024)`) | `crates/susi-gmcp/src/mcp/service.rs` (shipped) |
| Per-session response sink buffer | `64` messages (`mpsc::channel(64)`) | `crates/susi-gmcp/src/mcp/service.rs` (shipped) |
| Progress minimum dispatch interval | `PROGRESS_GAP = 100ms` (~10 notifications/sec max, trailing coalesce) | `crates/susi-gmcp/src/protocol.rs` (shipped) |
| Maximum RPC wire line / HTTP body | `16 MiB` stdio line / `4 MiB` HTTP request body / `8 MiB` resource read | `mcp/transport.rs`, `mcp/http.rs`, `protocol.rs` (shipped) |
| Concurrent active `subscriptions/listen` per session | `<= 16` streams; `<= 256` URIs per `resourceSubscriptions` filter | Target cap for `T-CODEXGMCPROADMAP-94` |
| Operational log & progress notification payload ceiling | `<= 16 KiB` redacted `data`/`message` per notification; bounded burst rate per request | Target cap for `T-CODEXGMCPROADMAP-94` / `-96` |
| Cancellation teardown & shutdown completion | Immediate loop exit on `context.cancelled()`; HTTP keep-alive `15s`, SSE retry `3s`, EOF drain `5s` (`2s` cancel) | `protocol.rs`, `mcp/service.rs`, `mcp/http.rs` (shipped baseline) |

## Fixtures

### Positive fixtures (contracted pass behavior)

1. **`F-POS-012-01` Subscription handshake & filter isolation**: Modern `subscriptions/listen` requesting `toolsListChanged` + `resourceSubscriptions: ["watch://a"]` emits `notifications/subscriptions/acknowledged` carrying matching `_meta["io.modelcontextprotocol/subscriptionId"]` strictly before any update notification, delivers `watch://a` updates and `tools/list_changed`, and suppresses `prompts/list_changed` and `watch://b`.
2. **`F-POS-012-02` Multiplexed concurrent subscriptions**: Two concurrent `subscriptions/listen` requests (`id: 10`, `id: 11`) demultiplex cleanly via `_meta["io.modelcontextprotocol/subscriptionId"]`; cancelling `10` via `notifications/cancelled` preserves `11`.
3. **`F-POS-012-03` Graceful closure & stateless reconnect**: Server shutdown emits `resultType: "complete"` with `_meta["io.modelcontextprotocol/subscriptionId"]` on each open subscription; post-disconnect reconnect re-establishes cleanly only upon fresh `subscriptions/listen`.
4. **`F-POS-012-04` Request-correlated monotonic progress & rate coalescing**: Watched tool call with `_meta.progressToken = "tok-1"` emits `0.0`, intermediate `sent/(sent+1.0)` updates spaced `>= PROGRESS_GAP` (`100ms`), and `1.0` finish; task-augmented request preserves initial `progressToken` until terminal task completion.
5. **`F-POS-012-05` Per-request logging severity & redaction**: `2026-07-28` request with `_meta["io.modelcontextprotocol/logLevel"] = "notice"` receives only `notice..emergency` notifications on the request response stream, with credentials/secrets/PII scrubbed.
6. **`F-POS-012-06` Legacy `2025-11-25` adapter parity**: Negotiated `2025-11-25` session honors `resources/subscribe`, `resources/unsubscribe`, and `logging/setLevel` through the compatibility adapter.

### Negative fixtures (contracted fail-closed behavior)

1. **`F-NEG-012-01` Unauthorized / dynamically revoked resource subscription**: Requesting or retaining `resourceSubscriptions` for a URI denied by `ResourceGuard` / `admit_resource` omits denied URIs at ack time and suppresses subsequent `Change::Resource` emissions after runtime revocation.
2. **`F-NEG-012-02` Fan-out cap & slow-consumer lag**: Opening more than `16` concurrent `subscriptions/listen` streams in a session rejects excess requests deterministically; lagging past the `256`-event `broadcast` buffer closes the lagged subscription cleanly instead of dropping silently or growing memory unboundedly.
3. **`F-NEG-012-03` Progress token abuse & post-terminal silence**: Reusing an already active `progressToken` is rejected; cancelling via `notifications/cancelled` halts both trailing progress updates and `1.0` completion notification.
4. **`F-NEG-012-04` Missing or invalid `io.modelcontextprotocol/logLevel` & stream leakage**: Request omitting `_meta["io.modelcontextprotocol/logLevel"]` emits zero `notifications/message`; request passing an unrecognized log level string (`"verbose"`) fails with `-32602`; `notifications/message` never traverses a `subscriptions/listen` stream.
5. **`F-NEG-012-05` Covert channel & secret smuggling attempt**: Tool or logger emitting secret material (`API_KEY=...`, bearer token, PII) or oversized payloads in progress/log fields is redacted and bounded before wire serialization.
6. **`F-NEG-012-06` Modern call to removed legacy RPCs**: Calling `resources/subscribe`, `resources/unsubscribe`, or `logging/setLevel` on a `2026-07-28` session returns `-32601` Method not found (once `-94` gates `logging/setLevel`).

## Evidence locations

- **Normative contract**: `docs/gmcp/contract-203-012.md`
- **Pinned sources and requirements ledger**: `.agents/gmcp/sources.json`, `.agents/gmcp/requirements-ledger.json`
- **Contract verification test**: `tests/gmcp_requirements.rs` (`gmcp_req_203_012_01_contract`, acceptance target for `T-CODEXGMCPROADMAP-93`)
- **Vector deliverable tests (`T-CODEXGMCPROADMAP-94`..`99`)**: `tests/gmcp_requirements.rs` (`gmcp_req_203_012_02_production`, `gmcp_req_203_012_03_integration`, `gmcp_req_203_012_04_adversarial`, `gmcp_req_203_012_05_interop`, `gmcp_req_203_012_06_operations`, `gmcp_vc_203_012_mastery`)
- **Current local evidence**: `.susi/records/gmcp/evidence/VC-203-012.jsonl` and `.agents/roadmap-verdicts/VC-203-012.json` (generated and hashed during `T-CODEXGMCPROADMAP-98` and `T-CODEXGMCPROADMAP-99`)

## Ambiguities resolved

1. **Partial vs all-or-nothing `SubscriptionFilter` rejection**: Resolved in favor of capability + `admit_resource` policy intersection: unsupported capabilities and unauthorized resource URIs are omitted from `notifications/subscriptions/acknowledged.params.notifications` rather than failing permitted keys in the same `subscriptions/listen` request.
2. **Dynamic ACL revocation on active subscriptions**: Resolved to enforce `admit_resource` both at `accepted_subscription_filter` negotiation AND immediately before sending each `Change::Resource(uri)` notification in `listen()`.
3. **Slow-subscriber replay vs termination**: Resolved as fail-closed teardown (`broadcast::RecvError::Lagged` terminates the lagged subscription with explicit error rather than unboundedly queueing or silently omitting events; clients re-subscribe via `subscriptions/listen` and re-fetch authoritative state via `*/list` or `resources/read`).
4. **Notification stream isolation for logging**: Resolved strictly per `2026-07-28` (`MCP-2026-0728-server-utilities-logging-mdx-0069`): `notifications/message` MUST attach only to the originating request response stream carrying `_meta["io.modelcontextprotocol/logLevel"]`, NEVER onto a `subscriptions/listen` stream.
5. **Malformed `_meta["io.modelcontextprotocol/logLevel"]` handling**: Resolved to validate raw `_meta` value presence and reject non-RFC-5424 values with JSON-RPC `-32602` Invalid params (`MCP-2026-0728-server-utilities-logging-mdx-0101`), rather than ignoring malformed fields as absent.
6. **RFC 5424 severity floor comparison**: Resolved across the ordered 8-level syslog ladder (`debug < info < notice < warning < error < critical < alert < emergency`), emitting events whose severity `>=` requested threshold, never emitting when omitted.
7. **Legacy `logging/setLevel` vs modern deprecation (`SEP-2577`)**: Resolved to gate `logging/setLevel` strictly to legacy (`2025-11-25`) sessions (`-32601` on `2026-07-28`), keep `notifications/message` strictly guarded and redacted for requested compatibility, and recommend `stderr` (stdio) or OpenTelemetry on new Susi paths.
8. **Reconnect persistence**: Resolved as stateless across transport drops on both stdio and HTTP (`MCP-2026-0728-basic-patterns-subscriptions-mdx-0161` / `basic/patterns/index.mdx`): the server retains zero durable subscription state across reconnects; clients MUST re-issue `subscriptions/listen`.

## Operations and non-claims

- **Rollout & rollback boundary**: Existing `GmcpService::session()` isolates legacy `log_level` and `legacy_subscriptions` per transport session; any regressions in modern enforcement (`T-CODEXGMCPROADMAP-94..98`) remain cleanly revertible without altering on-disk user state, while legacy clients continue operating on negotiated `2025-11-25` adapter paths during the deprecation window.
- **Historical non-claims at contract stage (`T-CODEXGMCPROADMAP-93`)**: At contract freeze time (`T-CODEXGMCPROADMAP-93`), `gmcp_req_203_012_01_contract` pinned every requirement and design invariant prior to `T-CODEXGMCPROADMAP-94..98` runtime enforcement, differential/interop proof, and downstream `.susi/records/gmcp/evidence/VC-203-012.jsonl` sealing (`T-CODEXGMCPROADMAP-99`). Those runtime controls (`gmcp_req_203_012_02..06`) are now implemented and verified; final vector-level mastery sealing (`gmcp_vc_203_012_mastery` and `.agents/roadmap-verdicts/VC-203-012.json`) remains scoped to `T-CODEXGMCPROADMAP-99`.

## Operations and incident runbook

### Inspectable state

- `susi mcp ledger` is the production ledger verification CLI command. It emits a bounded JSON report exposing `rows`, `documents`, `revisions`, `dispositions`, `categories`, `vectors`, and `verified`; `ledger_verify` tool calls return identical counts and fail non-zero if `verified` is `false`.
- `susi mcp status` emits the inspectable MCP server control-plane state (`LeadingMcpManager::status`) over JSON without side effects.
- `susi mcp ledger --drift --refetched <dir>` executes deterministic, air-gapped standards drift detection across all 6 pinned `VC-203-012` subscription, progress, and operational logging specifications against `.agents/gmcp/sources.json`.
- Live runtime inspection across all served transports (`embedded_adapter`, `#[cfg(unix)]` Unix-domain socket adapter, `susi mcp serve` stdio, and Streamable HTTP `/mcp`):
  - `server/discover` and `initialize` advertise `serverInfo.name = "susi-gmcp"`, `ttlMs`, `capabilities.logging = {}`, `capabilities.tools.listChanged = true`, `capabilities.prompts.listChanged = true`, and `capabilities.resources = { subscribe: true, listChanged: true }`.
  - Opening `subscriptions/listen` emits `notifications/subscriptions/acknowledged` carrying `_meta["io.modelcontextprotocol/subscriptionId"]` and the exact capability- and `ResourceGuard`-intersected `SubscriptionFilter` (`toolsListChanged`, `promptsListChanged`, `resourcesListChanged`, `resourceSubscriptions`).

### Configuration validation

- CLI flag validation rejects inconsistent drift arguments fail-closed (`susi mcp ledger --refetched <dir>` without `--drift` exits non-zero).
- Schema validation (`susi_config::validate::validate_dir`) inspects `~/.susi/config.json` before startup and reports blocking `ConfigIssueKind::TypeMismatch` findings when `max_rpc_body_bytes` is non-numeric or config JSON is malformed.
- `GmcpServer::run_stdio` rejects `max_rpc_body_bytes == 0` prior to reading requests, emitting `[GMCP] Invalid max_rpc_body_bytes (0): must be > 0; refusing stdio transport` on `stderr` with zero bytes emitted on `stdout`; `http_service` logs `[GMCP] Invalid max_rpc_body_bytes (0); using bundled default for HTTP transport` to `stderr` when `0`.

### Bounded metrics, logs and traces

- Compiled ceiling invariants in `crates/susi-gmcp/src/{protocol.rs,mcp/service.rs,mcp/transport.rs,mcp/http.rs}` prevent unbounded queues, covert channels, and fan-out DoS:
  - Concurrent active `subscriptions/listen` streams per session: `MAX_ACTIVE_SUBSCRIPTIONS_PER_SESSION = 16`.
  - Resource URIs per `SubscriptionFilter.resourceSubscriptions`: `MAX_RESOURCE_SUBSCRIPTIONS_PER_FILTER = 256`.
  - Operational `notifications/message` emissions per request: `MAX_LOG_NOTIFICATIONS_PER_REQUEST = 64`; redacted `data`/`message` payload ceiling: `MAX_NOTIFICATION_PAYLOAD_BYTES = 16384` (16 KiB).
  - Broadcast event buffer: `256` (`Change`); per-session outbound notification queue: `1024`; response `sink_tx`: `64`; `PROGRESS_GAP = 100ms` coalescing interval; stdio frame limit `16 MiB`; HTTP body limit `4 MiB`; resource read limit `8 MiB`; HTTP keep-alive `15s` / SSE `retry: 3000ms`.
- Modern `2026-07-28` operational logs follow total RFC 5424 severity ordering (`debug`..`emergency`), are completely suppressed unless `_meta["io.modelcontextprotocol/logLevel"]` is present, never appear on `subscriptions/listen` streams, and scrub credentials/Bearer tokens/API keys/paths through `sanitize_logging_param` and `sanitize_progress_param`.
- Transport lifecycle diagnostics write exclusively to `stderr` (keeping `stdout` strict newline-delimited JSON-RPC); HTTP `/mcp` accepts W3C `traceparent` headers without bypassing `MCP-Protocol-Version`, `Mcp-Method`, or JSON-RPC schema validation.

### Diagnostics and incident triage

- `-32602` (`Invalid params`): unrecognized `_meta["io.modelcontextprotocol/logLevel"]`, exceeding `16` concurrent active `subscriptions/listen` streams or `256` URIs in `resourceSubscriptions`, duplicate active `progressToken`, empty string `progressToken`, non-finite or negative `progress`/`total`, or legacy `resources/subscribe` targeting an unknown/unadmitted resource.
- `-32601` (`Method not found`): calling legacy `logging/setLevel`, `resources/subscribe`, or `resources/unsubscribe` on a modern `2026-07-28` session, or calling modern `subscriptions/listen` on a negotiated `2025-11-25` adapter session.
- `-32600` (`Invalid Request`): injecting modern `_meta` into a `2025-11-25` legacy session, malformed JSON-RPC envelope, `BoundedLines` oversized line framing, or post-cancellation progress emissions.
- `-32603` (`Internal error`): slow subscriber whose outbound/broadcast lag overflows the `256`-event buffer (`RecvError::Lagged`) triggers immediate fail-closed subscription termination with `-32603` so healthy sibling streams stay unaffected.
- `-32020` / `-32021` / `-32022`: HTTP `400 Bad Request` rejections on `/mcp` for header/body mismatches (`HEADER_MISMATCH` on `Mcp-Method` or `Mcp-Name`), missing modern client capabilities (`MISSING_REQUIRED_CLIENT_CAPABILITY`), or unsupported protocol versions (`UNSUPPORTED_PROTOCOL_VERSION` returning `data.supported`).
- `hash_drift` / `unfetchable`: named findings emitted by `susi mcp ledger --drift --refetched <dir>` whenever any of the 6 pinned `VC-203-012` specs mutates or disappears relative to `.agents/gmcp/sources.json`:
  - `docs/specification/2025-11-25/basic/utilities/progress.mdx` (`sha256: 35d27ca9ad8ee9e029ab84afc751de208f7732c97c7a5d792abbcc237ab72528`)
  - `docs/specification/2025-11-25/server/utilities/logging.mdx` (`sha256: 37cfde22e75d2444c9d796c2df636b96c1c9d486e64b109e38169f2d7f2cf82a`)
  - `docs/specification/2026-07-28/basic/patterns/index.mdx` (`sha256: 9e08dd8b295cacc59bd4e233e9e1d6bfcc149695c265d95a18e2fddc9ad3968d`)
  - `docs/specification/2026-07-28/basic/patterns/progress.mdx` (`sha256: 9f55aca08920c633f3a2a35e0c77533db08d3fd19ced43918ea7c57aa5f85409`)
  - `docs/specification/2026-07-28/basic/patterns/subscriptions.mdx` (`sha256: 8333cbc3280cad293e96b4a20c3200face8e185ebf58f1c4a51c35bb96654abd`)
  - `docs/specification/2026-07-28/server/utilities/logging.mdx` (`sha256: 4e77ec3257e07ad69bfd6ed902771f14d6498b60fe6c9c325b8ff4bfc35647a6`)

### Examples

```text
# 1. Inspect GMCP ledger verification and MCP adapter status
susi mcp ledger
susi mcp status

# 2. Run air-gapped standards drift detection against vendored MCP subscription/progress/logging specs
susi mcp ledger --drift --refetched .agents/gmcp/spec

# 3. Verify VC-203-012 operations gate and refresh current local evidence
cargo test --offline --locked --test gmcp_requirements gmcp_req_203_012_06_operations
local-release record-gmcp
local-release record-gmcp --mastery
```

### Incident triage

Treat any `verified: false` drift finding (`hash_drift` or `unfetchable`), zero-byte `max_rpc_body_bytes` startup failure, `stdout` non-JSON frame contamination, leaked credential inside `notifications/message` or `notifications/progress`, missing `_meta["io.modelcontextprotocol/subscriptionId"]` on subscription events, or silent queue growth bypassing `-32603` slow-consumer termination as a release-blocking incident. Capture the `susi mcp ledger --drift` JSON output, `stderr` logs, `.susi/config.json`, failing request/response frames, and latest `.susi/records/gmcp/evidence/VC-203-*.jsonl` records before patching, then re-verify `gmcp_req_203_012`.

### Migration (SEP-2577 deprecation and re-pinning upstream specs)

1. **Client logging & subscription migration (`2025-11-25` -> `2026-07-28`)**: Move clients off deprecated `logging/setLevel` and `resources/subscribe`/`resources/unsubscribe` onto per-request `_meta["io.modelcontextprotocol/logLevel"]` plus `subscriptions/listen` (`SubscriptionFilter`); per SEP-2577, new Susi surfaces emit operational telemetry to `stderr` or OpenTelemetry rather than MCP `notifications/message`.
2. **Standards re-pinning**: Claim an isolated task (`susi-commercial tasks claim <id>`), update affected `.mdx` files under `.agents/gmcp/spec/` and SHA-256 digests in `.agents/gmcp/sources.json`, reconcile `.agents/gmcp/requirements-ledger.json` and `docs/gmcp/contract-203-012.md`, re-run `gmcp_req_203_012_01..06` and `check-gmcp-roadmap`, commit, and append fresh two-phase `local-release record-gmcp` proof records.

### Rollback

Specifications, contract prose, ledger bindings and tests are versioned Git artifacts; operational evidence uses local current snapshots, and subscription state across transports is intentionally stateless (no durable subscription store needs migration on downgrade). Revert any offending change with `git revert <commit>`, re-validate `~/.susi/config.json` (`susi_config::validate::validate_dir`), verify `susi mcp ledger --drift --refetched .agents/gmcp/spec` passes cleanly, re-run `gmcp_req_203_012_06_operations`, and refresh current local proof; optional history may be absent.

### Production guarantees and non-claims

- Production guarantees cover shipped `crates/susi-gmcp/src/{protocol.rs,jsonrpc_gate.rs,stdio.rs,server.rs,mcp/{http.rs,message.rs,model.rs,service.rs,transport.rs}}` and `crates/susi-config/src/secret_ref.rs` across `gmcp_req_203_012_01..06`.
- Non-claim: MCP subscription streams are strictly stateless across TCP/HTTP/process disconnects (`MCP-2026-0728-basic-patterns-subscriptions-mdx-0161`); servers do not buffer missed notifications for disconnected clients, and clients MUST re-issue `subscriptions/listen` and refresh authoritative state via `*/list` or `resources/read` upon reconnect.
- Non-claim: `notifications/message` remains retained only inside the SEP-2577 deprecation window for explicit compatibility requests carrying `_meta["io.modelcontextprotocol/logLevel"]` (modern) or negotiated `2025-11-25` adapter `logging/setLevel`; silent suppression is the default when omitted.
