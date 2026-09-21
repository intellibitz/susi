# susi

![SUSI Version](https://img.shields.io/badge/version-v0.4.1-blue.svg) ![License](https://img.shields.io/badge/license-Apache%202.0-green.svg)

**The operating system for AI agents.**

susi is an evidence-gated AI substrate that runs on your machine: a persistent host daemon, stable network ports external clients can hard-code, bearer-authenticated HTTP on those ports, local and cloud inference behind one router, and a swarm that refuses mission COMPLETE without absolute evidence. It sits on Linux / macOS / WSL (native Windows via `install.ps1`) — it does not replace your OS; it is the OS layer *for* agents.

> Mount models, agents, and MCP tools as capabilities. susi orchestrates a consensus **Swarm**, grounds work in structured **Evidence** (`EvidenceRecord` / `Claim` + live receipts), cross-examines with a **Truth** transformer against workspace reality, cryptographically **audits** actions into an immutable HMAC chain, **sandboxes** untrusted Wasm plugins/reflexes (Wasmer) and optional shell (Docker), distills routine intelligence into **reflexes**, and scouts and hot-plugs missing **MCP** tools at runtime.

---

## Host contract

External clients can trust these ports — the daemon never silently drifts them:

| Port | Surface |
|------|---------|
| **9090** | GMCP / MCP HTTP (`/mcp`; `/messages` alias) |
| **9091** | GEMI HTTP (inference / models) |
| **9092** | A2A UDP discovery |
| **9093** | GMCP HTTP alias (streamable / SSE) |

- **`global susi`** — background daemon bound to the host substrate (`~/.susi`), not to a project folder.
- **`susi` CLI** — jailed to the caller's cwd; intents run against that workspace while the daemon owns ports, models, and lock state.
- **Canonical binary** — `~/.susi/bin/susi` (hot-reloads when the binary hash changes).
- **Control plane** — `susi start` / `susi stop` are deterministic host commands (never missions). `start` waits until 9090–9093 are ready and prints the endpoints.
- **Zero-trust HTTP** — daemon seeds `~/.susi/api_token`; clients must send `Authorization: Bearer <token>` on 9090/9091/9093 (except `/health` / CORS preflight).

---

## The 8 pillars

Foundation claims — each must hold in source:

1. **Swarm** — multi-agent consensus (`susi-gawd`) that routes, debates, and converges on intents (split critical/healthy signals hard-reject; no rubber-stamp).
2. **Evidence** — structured `EvidenceRecord` / `Claim` trails plus live `ToolReceipt` ledger; no naked assertions (mission finals require absolute citations).
3. **Truth** — `TruthTransformer` cross-examines claims against workspace reality (absolute sources only: ledger citations, compiled reads, native verified receipts).
4. **Pluggable** — `CapabilityRegistry` mounts Candle / llama.cpp, vLLM / Ollama, OpenAI-compatible clouds, and MCP tools behind one interface.
5. **Audit** — agent actions signed into an immutable accountability chain (append-only, hash-linked HMAC-SHA256 under `~/.susi/audit.hmac.key`).
6. **Sandbox** — Wasmer isolates untrusted Wasm plugins and reflexes; Docker `sandbox_exec` isolates untrusted shell when available.
7. **Reflexes** — routine intelligence distilled into fast Wasm / tensor paths (`reflex_training_threshold`).
8. **Provision** — missing MCP servers and tools are scouted and hot-plugged at runtime.

---

## Install

### Linux / macOS / WSL
```bash
curl -sSfL https://raw.githubusercontent.com/intellibitz/susi/main/install.sh | bash
```

Pipe into `bash`, not `sh` (Debian/Ubuntu/Alpine `sh` is not bash). Binaries and model weights are checksum-verified; if no platform binary exists, the installer builds from source ([rustup.rs](https://rustup.rs)).

By default a login-persistent daemon is registered (systemd user unit / launchd). Skip with:
```bash
curl -sSfL https://raw.githubusercontent.com/intellibitz/susi/main/install.sh | SUSI_NO_DAEMON=1 bash
```

### Windows PowerShell (native, not WSL)
```powershell
irm https://raw.githubusercontent.com/intellibitz/susi/main/install.ps1 | iex
```

---

## Quickstart

```bash
# Bring up / tear down the host daemon (control plane — not a mission)
susi start
susi stop

# Task across the agent swarm (cwd = workspace)
susi "analyze this workspace and propose an optimization plan"

# Identity / status
susi identity
susi status

# Cloud keys (peer of models) — env or ~/.susi/cloud.env
susi keys set openai          # prompts, or pipe the key on stdin
susi keys list
susi keys prefer deepseek

# Point external MCP / HTTP clients at the host contract
#   http://127.0.0.1:9090/mcp
#   http://127.0.0.1:9091/
#   http://127.0.0.1:9093/mcp
# Bearer required (except /health):
#   Authorization: Bearer "$(cat ~/.susi/api_token)"
```

Stdio MCP for editors that prefer a subprocess:
```bash
susi mcp
```

---

## Design principles

Defined in `.agents/IDENTITY.md` (Design principles):
- **Autonomous by default** — plan and execute via swarm consensus, not step-by-step babysitting.
- **Grounded outputs** — claims checked against tools and workspace state.
- **Traceable reasoning** — thinking and tool calls are inspectable (`.susi/last_mission_trace.json`).
- **Concurrency-first** — Tokio, Rayon, Crossbeam, parking_lot on real hardware.

---

## License

Apache License 2.0. See [LICENSE](LICENSE). Provided as-is, without warranty; users run the engine and its agents at their own risk.
