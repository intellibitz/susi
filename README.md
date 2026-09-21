# susi

![SUSI Version](https://img.shields.io/badge/version-v0.4.2-blue.svg) ![License](https://img.shields.io/badge/license-Apache%202.0-green.svg)

**The OS layer for AI agents** — an evidence-gated agent-of-agents substrate on your machine (not a replacement host OS).

susi runs a persistent host daemon, stable network ports external clients can hard-code, bearer-authenticated HTTP on those ports, local and cloud inference behind one router, and a swarm that refuses mission COMPLETE without absolute evidence. It sits on Linux / macOS / WSL (native Windows via `install.ps1`).

> Mount models, agents, and MCP tools as capabilities — open OpenAI-compat / MCP / protocol-peer admission (config or `susi mcp-add`), including the top external coding agents (Claude Code, Cursor, Codex, Roo Code, Cline, Devin, Manus, Qwen-Agent, OpenHands, GitHub Copilot) managed end-to-end via `susi agents`, plus top agent frameworks/engines (LangGraph, OpenAI Agents SDK, AutoGen, CrewAI, Qwen-Agent, Semantic Kernel, OpenHands Runtime, LangChain, PydanticAI, LlamaIndex) via `susi frameworks` when packages/entry configs are present. susi orchestrates a consensus **Swarm**, grounds work in structured **Evidence** (`EvidenceRecord` / `Claim` + live receipts), cross-examines with a **Truth** transformer against workspace reality, cryptographically **audits** actions into an immutable HMAC chain, **sandboxes** untrusted Wasm plugins/reflexes (Wasmer) and optional shell (Docker), distills routine intelligence into **reflexes**, and scouts and hot-plugs missing **MCP** tools at runtime. **Automation** is first-class via `susi automate <intent>`. Zero-config discovery runs on daemon start (engines / MCP / Candle fallback); install + optional API keys still apply.

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
- **Control plane** — `susi start` / `susi stop` / `susi restart` are deterministic host commands (never missions). `start`/`restart` wait until 9090–9093 are ready and print the endpoints.
- **Zero-trust HTTP** — daemon seeds `~/.susi/api_token`; clients must send `Authorization: Bearer <token>` on 9090/9091/9093 (except `/health` / CORS preflight).

---

## The 8 pillars

Foundation claims — each must hold in source:

1. **Swarm** — multi-agent consensus (`susi-gawd`) that routes, debates, and converges on intents (split critical/healthy signals hard-reject; no rubber-stamp).
2. **Evidence** — structured `EvidenceRecord` / `Claim` trails plus live `ToolReceipt` ledger; no naked assertions (mission finals require absolute citations).
3. **Truth** — `TruthTransformer` cross-examines claims against workspace reality (absolute sources only: ledger citations, compiled reads, native verified receipts).
4. **Pluggable** — Curated catalogs: ~10 peer executors + ~10 agent frameworks, ~15 inference engines, ~50 models (+ live `/models` discovery), ~100 real MCP packages (+ remote scout) — open-admitted when drivers/keys speak supported protocols (`managed`/`cli`/`openai_chat`/`http`/`a2a`, OpenAI-compat (+ Anthropic/Gemini/Triton), MCP stdio/HTTP). Top executors via `susi agents`; frameworks via `susi frameworks`. Local Candle: llama/qwen2 GGUF only.
5. **Audit** — agent actions signed into an immutable accountability chain (append-only, hash-linked HMAC-SHA256 under `~/.susi/audit.hmac.key`).
6. **Sandbox** — Wasmer isolates untrusted Wasm plugins and reflexes; Docker `sandbox_exec` isolates untrusted shell when available.
7. **Reflexes** — routine intelligence distilled into fast Wasm / tensor paths (`reflex_training_threshold`).
8. **Provision** — missing MCP servers/tools are scouted and hot-plugged; daemon `bootstrap_zero_config_substrate` probes engines/MCP and keeps Candle as fallback (install + optional keys still required — not zero setup).

---

## Verified product claims

These are the claims that hold in source (not marketing unbounded “any protocol”):

| Claim | Holds as |
|-------|----------|
| Agent-of-agents / OS layer for AI | Multi-agent swarm orchestrator + host daemon/ports — layer *for* agents, not a host OS |
| Plugin protocol agents | **~10** curated executors (`susi agents`) + **~10** frameworks (`susi frameworks`) in `external_peer_agents` (`managed`); UNAVAILABLE until driver/config present; open admission beyond the catalog |
| Plugin protocol engines | **~15** inference `inference_endpoints` (+ discovered local ports) — distinct from agent frameworks |
| Plugin protocol models | **~50** curated catalog + unbounded live `/models` discovery; local GGUF = llama/qwen2 |
| Plugin protocol MCP | **~100** real scout packages (+ remote registry refresh); any extra via `susi mcp-add` |
| Swarm / Truth / Evidence | `susi-gawd` swarm, `TruthTransformer`, `EvidenceSession` / `EvidenceRecord` |
| Automation | `susi automate <intent>` (evidence-gated swarm mission) |
| Zero-config discovery | Daemon probes engines/MCP + Candle fallback without per-engine edits |

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
susi restart

# Task across the agent swarm (cwd = workspace)
susi "analyze this workspace and propose an optimization plan"

# Identity / status
susi identity
susi status

# Cloud keys (peer of models) — env or ~/.susi/cloud.env
susi keys set openai          # prompts, or pipe the key on stdin
susi keys list
susi keys prefer deepseek

# Automation (evidence-gated swarm mission)
susi automate "refactor the auth module and verify with tests"

# Open-admit any MCP server (stdio or HTTP URL)
susi mcp-add filesystem npx -y @modelcontextprotocol/server-filesystem /tmp
susi mcp-add remote-http http://127.0.0.1:3100/mcp

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
