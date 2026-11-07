# Integrating with Susi

Susi is an Apache-2.0 AI OS substrate for a single owner on one host. Products
build on it from the outside, through public protocols on a fixed set of ports.
They never fork it. This guide lists those surfaces and how to verify an
integration against them.

## The host contract

A running host serves five contract ports. They move together by
`port_offset` (config) or `SUSI_PORT_OFFSET` (environment), and never
individually.

| Port | Protocol | Use it for |
|---|---|---|
| 9090 | MCP Streamable HTTP, `POST /mcp` | Tools, resources, prompts |
| 9091 | OpenAI-compatible HTTP | `/v1/chat/completions`, `/v1/completions`, `/v1/embeddings`, `/v1/models`; Anthropic Messages `/v1/messages`; Prometheus `/metrics` |
| 9092 | UDP discovery, cluster members only | Answers cluster-key signed pings between Susi members; unsigned pings get no reply. Products discover the host through `/.well-known/agent-card.json` on 9094 (see [host-contract-conformance.md](host-contract-conformance.md)). |
| 9093 | MCP alias of 9090 | Clients that need a second MCP endpoint |
| 9094 | A2A HTTP | `/` JSON-RPC, `/stream` SSE, `/.well-known/agent-card.json` |

## Authentication

Every HTTP contract request needs `Authorization: Bearer <token>`. The token
is the content of `api_token` in the host's home: `$SUSI_HOME/api_token`, by
default `~/.susi/api_token`. A missing or wrong token gets `401`. Three
requests are public:

- `GET /health` on 9090, 9091, 9093 and 9094, for supervisors and load
  balancers.
- `GET /.well-known/agent-card.json` on 9094, for A2A discovery.
- CORS preflight (`OPTIONS`).

## MCP (9090, 9093)

Both MCP revisions are served on `/mcp`:

- **2026-07-28 (current).** Stateless requests. Send `MCP-Protocol-Version:
  2026-07-28` and an `Mcp-Method: <method>` header, and put
  `io.modelcontextprotocol/protocolVersion` in `params._meta`. Start with
  `server/discover`.
- **2025-06-18 (session).** Call `initialize`, keep the returned
  `Mcp-Session-Id`, send `notifications/initialized`, then send requests on
  that session with `MCP-Protocol-Version: 2025-06-18`.

Send `Accept: application/json, text/event-stream`, because replies may arrive
as SSE. An unknown method returns JSON-RPC `-32601`. `tools/list` is paginated
and answers without waiting for external MCP servers, which are listed as
`server:*` until a background probe resolves their tool names.

```bash
# Example: Stateless MCP 2026-07-28 tools/list call
TOKEN="$(cat ~/.susi/api_token)"
curl -sS -X POST http://127.0.0.1:9090/mcp \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -H "MCP-Protocol-Version: 2026-07-28" \
  -H "Mcp-Method: tools/list" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}'
```

## A2A (9094)

Fetch the agent card from `/.well-known/agent-card.json`, then call JSON-RPC
on `/` with the bearer token. Errors use the standard JSON-RPC codes: `-32601`
for an unknown method and `-32602` for invalid params.

```bash
# Public discovery (no bearer token required)
curl -sS http://127.0.0.1:9094/.well-known/agent-card.json
```

## OpenAI-compatible inference (9091)

Point an OpenAI client at `http://127.0.0.1:9091/v1` with the bearer token as
its API key. `/v1/models` lists local models by file stem (never a local
path), configured providers, and the aliases `susi` and `auto`. Echo an `id`
from that list back as `model`.

```bash
# Example: Chat completion with inspectable placement routing
TOKEN="$(cat ~/.susi/api_token)"
curl -sS -X POST http://127.0.0.1:9091/v1/chat/completions \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "auto",
    "messages": [{"role": "user", "content": "Summarize the host contract ports."}],
    "susi": {"allow_cloud": true}
  }'
```

## Extensions

Extension packs add capabilities, catalogs and routing without changing
Susi. Core ships and seeds no pack and never installs one on its own: the user
or Susi Commercial Apps installs it, and core admits it. A third party writes
its own adapter. Core does not include that adapter. See [extension-pack.md](extension-pack.md) for the manifest and a 5-minute starter walkthrough. Packs live
in `~/.susi/extensions/<id>/`, declare an `apiVersion` whose major must match
the host's, and run only with the permissions they declare. Check a pack before you ship it with
`susi extensions validate <pack-dir>`: it applies the host's admission rules
and changes nothing on the host.

## Verify your integration

Run the black-box conformance suite against the host you target, locally or
in your CI:

```text
cargo run -p susi-conformance -- --port-offset <N> --token-file <home>/api_token
```

It checks health, auth, both MCP revisions, A2A discovery and error codes,
A2A `/stream` error codes, UDP discovery, and the model list, and reports the latency of each check. See
[host-contract-conformance.md](host-contract-conformance.md).

To test against a host without touching the one you use every day, give it
its own home and offset:

```text
SUSI_HOME=/tmp/susi-it SUSI_PORT_OFFSET=700 susi start
SUSI_HOME=/tmp/susi-it SUSI_PORT_OFFSET=700 susi stop
```

## Portable source targets

macOS is a portable source target using `Security.framework`, Linux uses
platform OpenSSL, and Windows uses `Schannel` (verified by
`tests/zc_matrix.rs`). Developers on any portable source target can compile and
run the conformance suite or work with the workspace directly via Cargo
(`cargo test --locked`).

## What stays outside this repository

Susi keeps single-host developer capabilities. Organisation-scale features,
such as multi-tenant isolation, SSO/RBAC, fleet management, central spend
governance and packaged Kubernetes deployment, are built outside it on these
same public surfaces, and so are every UI (the desktop and any visual view)
and every curated plugin pack: those live in Susi Commercial Apps (see "Open-core boundary" in `AGENTS.md`). A request
that names a tenant fails closed on the open substrate.
