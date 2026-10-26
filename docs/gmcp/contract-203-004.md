# VC-203-004 contract

This contract binds every normative clause of vector VC-203-004. Each row in the behavior table is normative; its identifier, level, clause and production path are pinned.

## Normative clause

The MVP must implement the 108 pinned clauses below. Every row is normative, bound to its production path, and resolved before implementation.

## Pinned sources

| repository | document | sha256 | lines |
|---|---|---|---|
| modelcontextprotocol | docs/specification/2025-11-25/basic/lifecycle.mdx | 45a6e8b7fb8c96e7b9ba1b0a3c727e8451c1e55bf56bb62f3ab63fddc365b919 | 40-265 |
| modelcontextprotocol | docs/specification/2025-11-25/basic/utilities/ping.mdx | f21b707244cd43bf4a562c2016eb91725db28c6f17eb3b279d1a8dffd415a463 | 29-66 |
| modelcontextprotocol | docs/specification/2025-11-25/basic/utilities/tasks.mdx | bef1bef9f939e09eed8f1928da4d3b36924f4a43b72ffc47a5c1e673f1c1a23b | 35-898 |
| modelcontextprotocol | docs/specification/2026-07-28/deprecated.mdx | 6ec5fc903d1be37e5ab0afda2acd7765aaf2909b90a2dca405e1aaea8936225b | 13-14 |

## Canonical data model

Each row names a canonical behavior; its ledger id is the canonical name and its production path is the only implementation site. Requests, responses and handles are explicit data, never ambient state.

## Behavior table

| id | level | clause | production path |
|---|---|---|---|
| MCP-2025-1125-basic-lifecycle-mdx-0040 | must | The initialization phase **MUST** be the first interaction between client and server. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-lifecycle-mdx-0047 | must | The client **MUST** initiate this phase by sending an `initialize` request containing: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-lifecycle-mdx-0098 | must | The server **MUST** respond with its own capabilities and information: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-lifecycle-mdx-0147 | must | After successful initialization, the client **MUST** send an `initialized` notification | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-lifecycle-mdx-0157 | should_not | - The client **SHOULD NOT** send requests other than | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-lifecycle-mdx-0160 | should_not | - The server **SHOULD NOT** send requests other than | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-lifecycle-mdx-0167 | must | In the `initialize` request, the client **MUST** send a protocol version it supports. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-lifecycle-mdx-0168 | should | This **SHOULD** be the _latest_ version supported by the client. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-lifecycle-mdx-0170 | must | If the server supports the requested protocol version, it **MUST** respond with the same | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-lifecycle-mdx-0171 | must | version. Otherwise, the server **MUST** respond with another protocol version it | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-lifecycle-mdx-0172 | should | supports. This **SHOULD** be the _latest_ version supported by the server. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-lifecycle-mdx-0174 | should | If the client does not support the version in the server's response, it **SHOULD** | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-lifecycle-mdx-0178 | must | If using HTTP, the client **MUST** include the `MCP-Protocol-Version: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-lifecycle-mdx-0217 | must | Both parties **MUST**: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-lifecycle-mdx-0230 | should | For the stdio [transport](/specification/2025-11-25/basic/transports), the client **SHOULD** initiate | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-lifecycle-mdx-0238 | may | The server **MAY** initiate shutdown by closing its output stream to the client and | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-lifecycle-mdx-0248 | should | Implementations **SHOULD** establish timeouts for all sent requests, to prevent hung | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-lifecycle-mdx-0250 | should | response within the timeout period, the sender **SHOULD** issue a [cancellation | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-lifecycle-mdx-0254 | should | SDKs and other middleware **SHOULD** allow these timeouts to be configured on a | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-lifecycle-mdx-0257 | may | Implementations **MAY** choose to reset the timeout clock when receiving a [progress | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-lifecycle-mdx-0259 | should | implies that work is actually happening. However, implementations **SHOULD** always | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-lifecycle-mdx-0265 | should | Implementations **SHOULD** be prepared to handle these error cases: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-ping-mdx-0029 | must | 1. The receiver **MUST** respond promptly with an empty response: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-ping-mdx-0039 | may | 2. If no response is received within a reasonable timeout period, the sender **MAY**: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-ping-mdx-0057 | should | - Implementations **SHOULD** periodically issue pings to detect connection health | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-ping-mdx-0058 | should | - The frequency of pings **SHOULD** be configurable | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-ping-mdx-0059 | should | - Timeouts **SHOULD** be appropriate for the network environment | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-ping-mdx-0060 | should | - Excessive pinging **SHOULD** be avoided to reduce network overhead | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-ping-mdx-0064 | should | - Timeouts **SHOULD** be treated as connection failures | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-ping-mdx-0065 | may | - Multiple failed pings **MAY** trigger connection reset | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-ping-mdx-0066 | should | - Implementations **SHOULD** log ping failures for diagnostics | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0035 | must | Servers and clients that support task-augmented requests **MUST** declare a `tasks` capability during initialization. The `tasks` capability is structured by request category, with boolean properties indicating which specific request types  | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0095 | should | During the initialization phase, both parties exchange their `tasks` capabilities to establish which operations support task-based execution. Requestors **SHOULD** only augment requests with a task if the corresponding capability has been d | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0099 | should_not | If `capabilities.tasks` is not defined, the peer **SHOULD NOT** attempt to create tasks during requests. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0113 | must_not | 1. If a server's capabilities do not include `tasks.requests.tools.call`, then clients **MUST NOT** attempt to use task augmentation on that server's tools, regardless of the `execution.taskSupport` value. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0115 | must_not | 1. If `execution.taskSupport` is not present or `"forbidden"`, clients **MUST NOT** attempt to invoke the tool as a task. Servers **SHOULD** return a `-32601` (Method not found) error if a client attempts to do so. This is the default behav | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0116 | may | 1. If `execution.taskSupport` is `"optional"`, clients **MAY** invoke the tool as a task or as a normal request. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0117 | must | 1. If `execution.taskSupport` is `"required"`, clients **MUST** invoke the tool as a task. Servers **MUST** return a `-32601` (Method not found) error if a client does not attempt to do so. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0128 | may | To create a task, requestors send a request with the `task` field included in the request params. Requestors **MAY** include a `ttl` value indicating the desired task lifetime duration (in milliseconds) since its creation. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0186 | may | In the Streamable HTTP (SSE) transport, clients **MAY** disconnect from an SSE stream opened by the server in response to a `tasks/get` request at any time. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0188 | must | While this note is not prescriptive regarding the specific usage of SSE streams, all implementations **MUST** continue to comply with the existing [Streamable HTTP transport specification](../transports#sending-messages-to-the-server). | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0193 | should | Requestors **SHOULD** respect the `pollInterval` provided in responses when determining polling frequency. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0195 | should | Requestors **SHOULD** continue polling until the task reaches a terminal status (`completed`, `failed`, or `cancelled`), or until encountering the [`input_required`](#input-required-status) status. Note that invoking `tasks/result` does not | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0232 | may | In the Streamable HTTP (SSE) transport, clients **MAY** disconnect from an SSE stream opened by the server in response to a `tasks/result` request at any time. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0234 | must | While this note is not prescriptive regarding the specific usage of SSE streams, all implementations **MUST** continue to comply with the existing [Streamable HTTP transport specification](../transports#sending-messages-to-the-server). | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0282 | may | When a task status changes, receivers **MAY** send a [`notifications/tasks/status`](/specification/2025-11-25/schema#notifications%2Ftasks%2Fstatus) notification to inform the requestor of the change. This notification includes the full tas | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0303 | must_not | Requestors **MUST NOT** rely on receiving this notifications, as it is optional. Receivers are not required to send status notifications and may choose to only send them for certain status transitions. Requestors **SHOULD** continue to poll | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0392 | must | 1. Receivers that do not declare the task capability for a request type **MUST** process requests of that type normally, ignoring any task-augmentation metadata if present. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0393 | may | 1. Receivers that declare the task capability for a request type **MAY** return an error for non-task-augmented requests, requiring requestors to use task augmentation. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0397 | must | 1. Task IDs **MUST** be a string value. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0398 | must | 1. Task IDs **MUST** be generated by the receiver when creating a task. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0399 | must | 1. Task IDs **MUST** be unique among all tasks controlled by the receiver. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0403 | must | 1. Tasks **MUST** begin in the `working` status when created. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0404 | must | 1. Receivers **MUST** only transition tasks through the following valid paths: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0407 | must_not | 1. Tasks with a `completed`, `failed`, or `cancelled` status are in a terminal state and **MUST NOT** transition to any other status | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0439 | should | Servers have flexibility in how they manage SSE streams during task polling and result retrieval, and clients **SHOULD** expect messages to be delivered on any SSE stream, including the HTTP GET stream. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0441 | should_not | Where possible, servers **SHOULD NOT** upgrade to an SSE stream in response to a `tasks/get` request, as the client has indicated it wishes to poll for a result. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0443 | must | While this note is not prescriptive regarding the specific usage of SSE streams, all implementations **MUST** continue to comply with the existing [Streamable HTTP transport specification](../transports#sending-messages-to-the-server). | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0447 | should | 1. When the task receiver has messages for the requestor that are necessary to complete the task, the receiver **SHOULD** move the task to the `input_required` status. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0448 | must | 1. The receiver **MUST** include the `io.modelcontextprotocol/related-task` metadata in the request to associate it with the task. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0449 | should | 1. When the requestor encounters the `input_required` status, it **SHOULD** preemptively call `tasks/result`. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0450 | should | 1. When the receiver receives all required input, the task **SHOULD** transition out of `input_required` status (typically back to `working`). | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0454 | must | 1. Receivers **MUST** include a `createdAt` [ISO 8601](https://datatracker.ietf.org/doc/html/rfc3339#section-5)-formatted timestamp in all task responses to indicate when the task was created. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0455 | must | 1. Receivers **MUST** include a `lastUpdatedAt` [ISO 8601](https://datatracker.ietf.org/doc/html/rfc3339#section-5)-formatted timestamp in all task responses to indicate when the task was last updated. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0456 | may | 1. Receivers **MAY** override the requested `ttl` duration. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0457 | must | 1. Receivers **MUST** include the actual `ttl` duration (or `null` for unlimited) in `tasks/get` responses. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0458 | may | 1. After a task's `ttl` lifetime has elapsed, receivers **MAY** delete the task and its results, regardless of the task status. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0459 | should | 1. Receivers **MAY** include a `pollInterval` value (in milliseconds) in `tasks/get` responses to suggest polling intervals. Requestors **SHOULD** respect this value when provided. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0463 | must | 1. Receivers that accept a task-augmented request **MUST** return a `CreateTaskResult` as the response. This result **SHOULD** be returned as soon as possible after accepting the task. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0464 | must | 1. When a receiver receives a `tasks/result` request for a task in a terminal status (`completed`, `failed`, or `cancelled`), it **MUST** return the final result of the underlying request, whether that is a successful result or a JSON-RPC e | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0465 | must | 1. When a receiver receives a `tasks/result` request for a task in any other non-terminal status (`working` or `input_required`), it **MUST** block the response until the task reaches a terminal status. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0466 | must | 1. For tasks in a terminal status, receivers **MUST** return from `tasks/result` exactly what the underlying request would have returned, whether that is a successful result or a JSON-RPC error. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0470 | must | 1. All requests, notifications, and responses related to a task **MUST** include the `io.modelcontextprotocol/related-task` key in their `_meta` field, with the value set to an object with a `taskId` matching the associated task ID. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0471 | must | 1. For example, an elicitation that a task-augmented tool call depends on **MUST** share the same related task ID with that tool call's task. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0472 | must | 1. For the `tasks/get`, `tasks/result`, and `tasks/cancel` operations, the `taskId` parameter in the request **MUST** be used as the source of truth for identifying the target task. Requestors **SHOULD NOT** include `io.modelcontextprotocol | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0473 | should_not | Similarly, for the `tasks/get`, `tasks/list`, and `tasks/cancel` operations, receivers **SHOULD NOT** include `io.modelcontextprotocol/related-task` metadata in the result messages, as the `taskId` is already present in the response structu | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0477 | may | 1. Receivers **MAY** send `notifications/tasks/status` notifications when a task's status changes. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0478 | must_not | 1. Requestors **MUST NOT** rely on receiving the `notifications/tasks/status` notification, as it is optional. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0479 | should_not | 1. When sent, the `notifications/tasks/status` notification **SHOULD NOT** include the `io.modelcontextprotocol/related-task` metadata, as the task ID is already present in the notification parameters. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0487 | should | 1. Receivers **SHOULD** use cursor-based pagination to limit the number of tasks returned in a single response. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0488 | must | 1. Receivers **MUST** include a `nextCursor` in the response if more tasks are available. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0489 | must | 1. Requestors **MUST** treat cursors as opaque tokens and not attempt to parse or modify them. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0490 | must | 1. If a task is retrievable via `tasks/get` for a requestor, it **MUST** be retrievable via `tasks/list` for that requestor. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0494 | must | 1. Receivers **MUST** reject cancellation requests for tasks already in a terminal status (`completed`, `failed`, or `cancelled`) with error code `-32602` (Invalid params). | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0495 | must | 1. Upon receiving a valid cancellation request, receivers **SHOULD** attempt to stop the task execution and **MUST** transition the task to `cancelled` status before sending the response. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0496 | must | 1. Once a task is cancelled, it **MUST** remain in `cancelled` status even if execution continues to completion or fails. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0497 | may | 1. The `tasks/cancel` operation does not define deletion behavior. However, receivers **MAY** delete cancelled tasks at their discretion at any time, including immediately after cancellation or after the task `ttl` expires. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0498 | should_not | 1. Requestors **SHOULD NOT** rely on cancelled tasks being retained for any specific duration and should retrieve any needed information before cancelling. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0742 | must | All requests, responses, and notifications associated with a task **MUST** include the `io.modelcontextprotocol/related-task` key in `_meta`: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0754 | should_not | For the `tasks/get`, `tasks/list`, and `tasks/cancel` operations, requestors and receivers **SHOULD NOT** include this metadata in their messages, as the `taskId` is already present in the message structure. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0755 | must | The `tasks/result` operation **MUST** include this metadata in its response, as the result structure itself does not contain the task ID. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0766 | must | Receivers **MUST** return standard JSON-RPC errors for the following protocol error cases: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0773 | may | Additionally, receivers **MAY** return the following errors: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0777 | should | Receivers **SHOULD** provide informative error messages to describe the cause of errors. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0839 | should | When the underlying request does not complete successfully, the task moves to the `failed` status. This includes JSON-RPC protocol errors during request execution, or for tool calls specifically, when the tool result has `isError` set to tr | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0862 | must | - If the underlying request resulted in a JSON-RPC error, `tasks/result` **MUST** return that same JSON-RPC error. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0863 | must | - If the request completed with a JSON-RPC response, `tasks/result` **MUST** return a successful JSON-RPC response containing that result. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0871 | must | When an authorization context is provided, receivers **MUST** bind tasks to said context. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0874 | should | In these scenarios, receivers **SHOULD** document this limitation clearly, as task results may be accessible to any requestor that can guess the task ID. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0875 | must | If context-binding is unavailable, receivers **MUST** generate cryptographically secure task IDs with enough entropy to prevent guessing and should consider using shorter TTL durations to reduce the exposure window. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0876 | should_not | Furthermore, receivers that cannot identify requestors **SHOULD NOT** declare the `tasks.list` capability, as listing tasks would expose task metadata to any requestor regardless of task ID entropy. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0878 | must | If context-binding is available, receivers **MUST** reject `tasks/get`, `tasks/result`, and `tasks/cancel` requests for tasks that do not belong to the same authorization context as the requestor. For `tasks/list` requests, receivers **MUST | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0880 | should | Additionally, receivers **SHOULD** implement rate limiting on task operations to prevent denial-of-service and enumeration attacks. | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0884 | should | 1. Receivers **SHOULD**: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0894 | should | 1. Receivers **SHOULD**: | crates/susi-gmcp/src/protocol.rs |
| MCP-2025-1125-basic-utilities-tasks-mdx-0898 | should | 1. Requestors **SHOULD**: | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-deprecated-mdx-0013 | should_not | removal: new implementations **SHOULD NOT** adopt it, and existing | crates/susi-gmcp/src/protocol.rs |
| MCP-2026-0728-deprecated-mdx-0014 | should | implementations **SHOULD** migrate before the feature's earliest removal. The | crates/susi-gmcp/src/protocol.rs |

## Compatibility and deprecation decisions

Every clause carries an explicit disposition: adapter=93, optional=15. A clause disposed as adapter is honored only through the explicit version adapter with downgrade telemetry and a tested removal policy.

- **Isolated compatibility adapters**: Dedicated adapters isolate legacy interactions (`initialize`/`initialized`, `ping`, legacy `HTTP+SSE`, resource `subscriptions`, `roots`, `sampling`, `elicitation`, `logging`, and legacy task shapes) without weakening the modern path.
- **Deterministic version selection & visible downgrade**: Modern requests carry `_meta` specifying `io.modelcontextprotocol/protocolVersion: 2026-07-28` and receive complete results with `resultType: complete`. Legacy sessions (`2025-11-25`) operate without modern envelopes.
- **Governed deprecation & error taxonomy**: Cross-era boundary violations earn deterministic typed errors: modern `_meta` on a legacy session returns `-32600`, while missing `_meta` on a modern session returns `-32602`. Telemetry tracks deprecated feature usage against removal policies.

## Threat model

A malicious peer can replay a handle, answer another principal's request, or stall past a deadline. Handles are principal-scoped and single-use; responses are validated against the request that created them; deadlines and cancellation are enforced before any side effect.

## SLOs and resource budgets

Every interaction completes within its declared deadline or is cancelled with a typed error. Bounded queues and per-principal handle budgets prevent unbounded growth.

## Fixtures

Positive and negative fixtures for every row live in .agents/gmcp/evidence/VC-203-004.jsonl, keyed by row id; the tests consume them.

## Evidence locations

.agents/gmcp/evidence/VC-203-004.jsonl holds the retained evidence; the acceptance test binds this document to the ledger.

## Ambiguities resolved

All clauses are resolved by this table: the ledger id is the canonical name, the production path is the only allowed implementation site, and the disposition decides whether the clause is honored on the modern path or only through the version adapter.

## Operations and incident runbook

### Inspectable state

- `susi mcp ledger` is the production health check. It emits one bounded
  JSON report containing `rows`, `documents`, `revisions`, `dispositions`,
  `categories`, `vectors`, and `verified`; a false verification exits
  non-zero before protocol traffic is trusted.
- `susi mcp call susi ledger_verify {}` exposes the same report through the
  tool surface. Comparing it with the CLI report is a transport-independent
  smoke check for direct, daemon, gateway, and stdio dispatch.
- Legacy (2025-11-25) and modern (2026-07-28) lifecycle operations are
  distinguishable on the served wire: legacy sessions negotiate era at
  initialize and omit modern result envelopes, while modern sessions require
  per-request protocol metadata and report `resultType: complete`.
- `susi mcp ledger --drift --refetched <dir>` is the deterministic, air-gapped
  check. The directory is keyed by `sources.json` document paths and is bounded
  by the same source and artifact limits as the live check.

### Diagnostics and incident triage

Every drift finding has a stable `kind`: `hash_drift` means bytes differ from
the pinned source, while `unfetchable` means a required document is absent or
could not be retrieved. A non-zero command and `verified: false` are required
for either condition; callers must not continue with a partially checked
corpus. For a local failure, restore the named vendored file from git. For an
upstream `hash_drift`, preserve the report and begin the migration below. For
an `unfetchable` source, retry within the published bounds, then recover the
last known pin from git history rather than silently skipping it.
Cross-era boundary violations earn deterministic typed errors: modern `_meta`
on a legacy session returns `-32600`, while missing `_meta` on a modern session
returns `-32602`. Deprecation policies ensure that legacy features are maintained
strictly through isolated adapters with telemetry and scheduled removal.

### Incident triage

Treat a non-zero report as an incident until the named source or pin is
restored and the deterministic check is green again. Preserve the JSON report
and the corresponding evidence record with the incident for independent
reproduction.

### Migration (re-pinning the lifecycle specifications)

1. Claim a task and record the upstream revision and reason for the re-pin.
2. Refresh `.agents/gmcp/spec/` and `sources.json` from that exact revision.
3. Run `scripts/check-gmcp-ledger.py --write`, review every changed
   lifecycle clause, and update fixtures, compatibility decisions, and the
   retained evidence record.
4. Run `susi mcp ledger`, the deterministic `--refetched` check, and the full
   GMCP acceptance gate before publishing the change.

### Rollback

The vendored specification, manifest, ledger, fixtures, and evidence are
committed artifacts. Roll back a bad re-pin with `git revert` of its commit,
then rerun the CLI and deterministic drift checks. This restores the prior
lifecycle and deprecation semantics without a data migration; do not delete
evidence rows or rewrite a prior proof event.

