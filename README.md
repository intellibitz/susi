# susi

![SUSI Version](https://img.shields.io/badge/version-v0.9.0-blue.svg) ![License](https://img.shields.io/badge/license-Apache%202.0-green.svg)

**The OS layer for AI agents** — a Tier S evidence-gated agent-of-agents substrate on your machine (not a replacement host OS). Verify the crown with `susi crown`.

susi runs a persistent host daemon, stable network ports external clients can hard-code, bearer-authenticated HTTP on those ports, local and cloud inference behind one router, and a swarm that refuses mission COMPLETE without absolute evidence. It sits on Linux / macOS / WSL (native Windows via `install.ps1`).

> Mount models, agents, and MCP tools as capabilities — open OpenAI-compat / MCP / protocol-peer admission (config or `susi mcp-add` / `susi mcp enable`), including top external coding agents via `susi agents`, agent frameworks via `susi frameworks`, coding/agent models via `susi models`, top frontier engines via `susi frontier`, open-weight local hosts via `susi openweight`, OpenRouter via `susi openrouter`, and leading MCP tool servers (Filesystem, GitHub, Context7, Playwright, Sentry + Chrome DevTools, …) via `susi mcp list|enable` when launchers/keys are present. susi orchestrates a consensus **Swarm**, grounds work in structured **Evidence** (`EvidenceRecord` / `Claim` + live receipts), cross-examines with a **Truth** transformer against workspace reality, cryptographically **audits** actions into an immutable HMAC chain, **sandboxes** untrusted Wasm plugins/reflexes (Wasmer) and optional shell (Docker), distills routine intelligence into **reflexes**, and scouts and hot-plugs missing **MCP** tools at runtime. **Automation** is first-class via `susi automate <intent>`. Zero-config discovery runs on daemon start (engines / MCP / Candle fallback); install + optional API keys still apply.

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

## Foundation pillars (Tier S)

Every pillar below is a **Tier S** crown USP — must hold in source and pass `susi crown verify`:

1. **Truth** — absolute sources only; models never certify.
2. **Evidence** — `EvidenceSession` / `ToolReceipt` ledger; no naked assertions.
3. **Swarm** — agent-of-agents consensus (GAWD).
4. **Blackboard** — live shared state (`.susi/last_blackboard.json`).
5. **Glass box** — inspectable traces / blackboard / governance / crown reports.
6. **Audit** — append-only HMAC accountability chain.
7. **Zero-config Auto** — `susi auto`; host installs/keys still gate cloud.
8. **Governance-first** — Safety/Security before parallel fleet.
9. **Pluggable** — `CapabilityRegistry` + extension packs; managed catalogs (`models` / `frontier` / `openweight` / `agents` / `frameworks` / `mcp` / `openrouter`).
10. **Sandbox** — Wasmer for Wasm; Docker `sandbox_exec` when available.
11. **Host contract** — fixed ports 9090–9093.
12. **Reflexes** — Wasm reflexes under the data dir.
13. **Provision** — daemon bootstrap + auto-prime + Candle/weight ladder.
14. **Concurrency-first** — Tokio / Rayon / Crossbeam / parking_lot.

Verify: `susi crown` (exit non-zero if any critical USP fails).

---

## Verified product claims

These are the claims that hold in source (not marketing unbounded “any protocol”):

| Claim | Holds as |
|-------|----------|
| Tier S crown | `susi crown verify` — Truth/Evidence/Swarm/Blackboard/Glass/Audit/Auto/Governance/Pluggable/Sandbox/Host/Reflexes/Provision/Concurrency |
| Agent-of-agents / OS layer for AI | Multi-agent swarm + host daemon/ports — layer *for* agents, not a host OS |
| Plugin protocol agents | **~10** executors + **~10** frameworks; UNAVAILABLE until driver/config present |
| Plugin protocol engines | **~15** inference endpoints (+ discovered local ports) |
| Plugin protocol models | **~10** coding + **5** frontier + **5** open-weight + OpenRouter routes + **~50** catalog + live `/models`; local GGUF / Ollama |
| Plugin protocol MCP | **~22** leading servers (top: Filesystem, GitHub, Context7, Playwright, Sentry) + **~100** scout packages |
| Automation | `susi automate <intent>` (evidence-gated swarm mission) |

## Install

### Linux / macOS / WSL
```bash
curl -sSfL https://raw.githubusercontent.com/intellibitz/susi/main/install.sh | bash
```

Pipe into `bash`, not `sh` (Debian/Ubuntu/Alpine `sh` is not bash). Binaries and model weights are checksum-verified; if no platform binary exists, the installer builds from source ([rustup.rs](https://rustup.rs)). Prefers a prebuilt release even inside a source checkout (set `SUSI_FORCE_SOURCE=1` to build that tree). Source builds reuse `~/.susi/build-cache` unless `SUSI_USE_REPO_TARGET=1` or `CARGO_TARGET_DIR` is set. On Linux, a missing mold linker is bootstrapped into `~/.susi/mold` (skip with `SUSI_SKIP_MOLD_BOOTSTRAP=1`).

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

Defined in `.agents/identity.json` (`design_principles`):
- **Autonomous by default** — plan and execute via swarm consensus, not step-by-step babysitting.
- **Grounded outputs** — claims checked against tools and workspace state.
- **Traceable reasoning** — thinking and tool calls are inspectable (`.susi/last_mission_trace.json`, `.susi/last_blackboard.json`).
- **Concurrency-first** — Tokio, Rayon, Crossbeam, parking_lot on real hardware.
- **Rust substrate** — first-party runtime is Rust (or Wasm from Rust); non-Rust peers are opt-in managed adapters only (Mandate 43), not a “100% Rust process tree” claim.

---

## License

Apache License 2.0. See [LICENSE](LICENSE). Provided as-is, without warranty; users run the engine and its agents at their own risk.
