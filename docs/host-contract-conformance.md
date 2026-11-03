# Host contract conformance

Products that plug into Susi talk to the five host-contract ports (see the
README's "Host contract"). `susi-conformance` checks a running host the way such
a product sees it. Run it before you ship an integration, and in your own CI
against the Susi version you support.

```text
cargo run -p susi-conformance -- [--host 127.0.0.1] [--port-offset N] [--token-file PATH] [--json]
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
| `gemi.models@9091` | `/v1/models` returns a `data` array. A **warning** flags model ids that are local filesystem paths. |

Every check reports its own wall-clock latency, so the suite doubles as a quick
responsiveness probe for a host.

## Scope

The suite is black-box: standard library HTTP/1.1 and JSON only, no Susi
internals. It does not start or stop the host and never sends goals,
credentials or model downloads. Port 9092 (A2A UDP discovery) is not yet
covered.
