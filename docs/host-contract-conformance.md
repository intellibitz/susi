# Host contract conformance

Products that plug into Susi talk to the five host-contract ports (see the
README's "Host contract"). `susi-conformance` checks a running host the way such
a product sees it. Run it before you ship an integration, and in your own CI
against the Susi version you support.

```text
cargo run -p susi-conformance -- [--host 127.0.0.1] [--port-offset N] [--token-file PATH]
    [--timeout-ms N] [--max-ms N] [--susi-bin PATH] [--inference] [--json]
```

The token defaults to `$SUSI_HOME/api_token`, else `~/.susi/api_token`. The
port offset defaults to `SUSI_PORT_OFFSET`. An explicit `--token-file` that holds no token is a usage error, so a broken setup cannot pass by skipping every authenticated check. Exit status: `0` conformant
(warnings allowed), `1` a check failed, `2` bad usage. `--json` prints one
machine-readable report.

## Checks

| Id | Contract |
|---|---|
| `health@<port>` | `GET /health` answers 200 **without** a token on 9090, 9091, 9093 and 9094, for load balancers and supervisors. |
| `auth.required@<port>` | A protected request without `Authorization: Bearer` is refused with 401 on every HTTP contract port. |
| `mcp.discover@<port>` | MCP `server/discover` at revision 2026-07-28 (stateless, `Mcp-Method` header) answers with capabilities and server info on 9090 and the 9093 alias. |
| `mcp.tools-list@9090` | `tools/list` returns a tool array. |
| `mcp.unknown-method@9090` | An unknown method is JSON-RPC `-32601`. |
| `mcp.session-2025-06-18@9090` | The session flow most deployed MCP clients use: `initialize` returns `Mcp-Session-Id`, `notifications/initialized` is accepted, and `tools/list` works on the session. |
| `a2a.agent-card@9094` | `/.well-known/agent-card.json` is public and names at least one interface URL. |
| `a2a.unknown-method@9094` | An unknown A2A JSON-RPC method is `-32601`, not a wrapped internal error. |
| `auth.required@9094/stream` | The SSE route `POST /stream` refuses a request without a token with 401, like `/`. |
| `a2a.stream@9094` | The SSE route `POST /stream` dispatches the streaming methods: an unknown method is `-32601`, and subscribing to a task that does not exist is `-32001` (task not found) under the A2A 1.0 name `SubscribeToTask` and the legacy 0.3 alias `tasks/resubscribe`. No task is started. |
| `udp.discovery@9092` | UDP discovery answers **only** bound cluster-key signed pings (`SUSI_PING_SIG3`: MAC over the sender's source address, a timestamp and a nonce). The suite holds no cluster key, so it checks the other half: the retired `SUSI_LAN_PING`, a bare `SUSI_PING`, the retired unbound `SUSI_PING_SIG` / `SUSI_PING_SIG2` and a bound ping with a forged MAC all draw silence, and the port is bound (no ICMP port-unreachable). On a non-loopback host silence cannot prove the port is bound, so the result is a **warning**. |
| `gemi.models@9091` | `/v1/models` returns a `data` array. A **warning** flags model ids that are local filesystem paths. |
| `gemi.stream@9091` | Opt-in (`--inference`). `stream: true` on `/v1/chat/completions` answers `text/event-stream` with JSON `data:` chunks and a final `data: [DONE]`. The request is 1 token and local-only (`susi.allow_cloud: false`). A host with no local inference target answers `503` "request prohibits cloud inference ..." instead: a **warning**, SSE not exercised. Any other non-SSE reply fails. |
| `mcp.stdio` | Opt-in (`--susi-bin PATH`). `PATH mcp serve` speaks MCP over newline-delimited stdio: `initialize` (2025-06-18) names the server and a revision, `notifications/initialized` is accepted, and `tools/list` returns a tool array. Stdout must carry only JSON-RPC: a banner or log line there fails the check. The child inherits the environment (`SUSI_HOME` selects the instance) and is stopped before the check returns. |

Every check reports its own wall-clock latency. `--max-ms N` turns that into a
budget: any passing check slower than N ms fails, so a latency regression
fails CI like a contract violation.

A2A JSON-RPC on 9094 (`/` and `/stream`) speaks protocol version 1.0, the
version the agent card advertises: the 1.0 method names (`SendMessage`,
`SendStreamingMessage`, `GetTask`, `ListTasks`, `CancelTask`,
`SubscribeToTask`, the push-notification-config methods and
`GetExtendedAgentCard`) are served, and the 0.3 slash names (`message/send`,
`tasks/resubscribe`, ...) stay accepted as legacy aliases.

Port 9093 is a permanent alias of 9090 for MCP: deployed clients hard-code
it, so it is checked like 9090 and will not be retired.

## Out of contract

These listen on the host but are **not** part of the host contract. They are
internal to Susi, may change or move between releases without notice, and
products must not depend on them. The suite does not check them.

| Surface | What it is |
|---|---|
| UDP 9095 | Cluster gossip between Susi members (not A2A discovery). |
| 18080-18084 | Leaf services: paths, error, config, sandbox and native. |
| 19090-19092 | Swarm Cell servers (gmcp, gemi and gawd cells). |
| Plane bus | IPC between Susi's own processes on ephemeral loopback ports, published under the cache directory. |
| 9180 | `susi-harness` metering gateway. |
| 9181 | Harness JSON/SSE API (`susi harness api`). |

The `x-susi` request signing and sealing used between cluster members is
likewise internal: standard Bearer clients never need it.

## Scope

The suite is black-box: standard library HTTP/1.1, UDP and JSON only, no Susi
internals. It does not start or stop the host and never sends goals,
credentials or model downloads. Two checks do more than read the host and run
only when asked: `--inference` (one local, 1-token completion) and
`--susi-bin` (starts and stops one `susi mcp serve` child).
