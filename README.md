# susi

![SUSI Version](https://img.shields.io/badge/version-v0.7.0-blue.svg) ![License](https://img.shields.io/badge/license-Apache%202.0-green.svg)

**The OS layer for AI agents** — an evidence-gated agent-of-agents substrate on your machine (not a replacement host OS).

susi runs a persistent host daemon, stable network ports external clients can hard-code, bearer-authenticated HTTP on those ports, local and cloud inference behind one router, and a swarm that refuses mission COMPLETE without absolute evidence. It sits on Linux / macOS / WSL (native Windows via `install.ps1`).

> Mount models, agents, and MCP tools as capabilities — open OpenAI-compat / MCP / protocol-peer admission (config or `susi mcp-add` / `susi mcp enable`), including the top external coding agents via `susi agents`, agent frameworks via `susi frameworks`, coding/agent models via `susi models`, and top MCP tool servers (GitHub, Filesystem, PostgreSQL, Browser, Docker, Git, Jira, Linear, Kubernetes, Slack) via `susi mcp list|enable` when launchers/keys are present. susi orchestrates a consensus **Swarm**, grounds work in structured **Evidence** (`EvidenceRecord` / `Claim` + live receipts), cross-examines with a **Truth** transformer against workspace reality, cryptographically **audits** actions into an immutable HMAC chain, **sandboxes** untrusted Wasm plugins/reflexes (Wasmer) and optional shell (Docker), distills routine intelligence into **reflexes**, and scouts and hot-plugs missing **MCP** tools at runtime. **Automation** is first-class via `susi automate <intent>`. Zero-config discovery runs on daemon start (engines / MCP / Candle fallback); install + optional API keys still apply.

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

## Foundation pillars

Claims that must hold in source (aligned with `.agents/IDENTITY.md`):

1. **Swarm** — multi-agent consensus that routes, debates, and converges on intents.
2. **Evidence** — structured `EvidenceRecord` / `Claim` trails plus live `ToolReceipt` ledger; no naked assertions.
3. **Truth** — `TruthTransformer` absolute sources only (ledger citations, compiled reads, native receipts).
4. **Blackboard** — live swarm shared state; persisted to `.susi/last_blackboard.json` (`susi blackboard`).
5. **Glass box** — inspectable reasoning, tools, blackboard, and auto-prime reports.
6. **Zero-config Auto** — auto-seed packs, enable ready MCP, prefer ready models, admit ready peers (`susi auto`); host installs/keys still gate cloud tiers.
7. **Pluggable** — curated catalogs + open admission via protocols; extension packs hold vendor opinions.
8. **Audit** — append-only HMAC accountability chain.
9. **Sandbox** — Wasmer for untrusted Wasm; Docker `sandbox_exec` for untrusted shell when available.
10. **Reflexes** — routine intelligence distilled into fast Wasm / tensor paths.
11. **Provision** — daemon bootstrap + auto-prime + Candle/weight ladder.

---

## Verified product claims

These are the claims that hold in source (not marketing unbounded “any protocol”):

| Claim | Holds as |
|-------|----------|
| Agent-of-agents / OS layer for AI | Multi-agent swarm orchestrator + host daemon/ports — layer *for* agents, not a host OS |
| Evidence / Truth / Blackboard / Glass box | Ledger + absolute truth gate + persisted blackboard in mission traces |
| Zero-config Auto | `susi auto` + bootstrap auto-prime packs/MCP/models/peers/engines (host keys/installs still required for cloud) |
| Plugin protocol agents | **~10** curated executors (`susi agents`) + **~10** frameworks (`susi frameworks`); UNAVAILABLE until driver/config present |
| Plugin protocol engines | **~15** inference `inference_endpoints` (+ discovered local ports) |
| Plugin protocol models | **~10** ranked coding/agent models (`susi models`) + **~50** catalog + live `/models`; local GGUF = llama/qwen2 |
| Plugin protocol MCP | **~10** leading tool servers (`susi mcp`) + **~100** scout packages; extras via `susi mcp-add` |
| Automation | `susi automate <intent>` (evidence-gated swarm mission) |

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
susi mcp list
susi mcp enable filesystem
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

## Extension packs

Core is **protocol-generic** (admit/run/doctor/enable + registries). Vendor opinions — cloud env aliases, curated catalog pointers, ranks — live in **extension packs**. Zero-config lifecycle:

- **Auto-seed** — first run creates `~/.susi/extensions/default/` from the bundled pack (never overwrites host edits)
- **Auto-load** — packs with `manifest.json` under `~/.susi/extensions/<id>/` are discovered and marked loaded; `state.json` tracks active id
- **CLI** — `susi extensions` / `susi ext` (`list`, `status`, `seed`, `create <id>`, `load <id>`, `unload <id>`)
- Bundled source: `config/extensions/default/`
- Force active pack: `SUSI_EXTENSION_PACK=<id>`

Catalog loaders still embed bundled JSON via `include_str!` for offline boot; host pack files win when present.

---

## Design principles

Defined in `.agents/IDENTITY.md` (Design principles):
- **Autonomous by default** — plan and execute via swarm consensus, not step-by-step babysitting.
- **Grounded outputs** — claims checked against tools and workspace state.
- **Traceable reasoning** — thinking and tool calls are inspectable (`.susi/last_mission_trace.json`, `.susi/last_blackboard.json`).
- **Concurrency-first** — Tokio, Rayon, Crossbeam, parking_lot on real hardware.

---

## License

Apache License 2.0. See [LICENSE](LICENSE). Provided as-is, without warranty; users run the engine and its agents at their own risk.
