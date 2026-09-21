# susi

![SUSI Version](https://img.shields.io/badge/version-v0.4.1-blue.svg) ![License](https://img.shields.io/badge/license-Apache%202.0-green.svg)

**The operating system for AI agents.**

susi is a zero-trust AI substrate that runs on your machine: a persistent host daemon, stable network ports external clients can hard-code, local and cloud inference behind one router, and a swarm that only accepts claims backed by evidence. It sits on Linux / macOS / WSL — it does not replace your OS; it is the OS layer *for* agents.

> Bring any model, any agent, any MCP tool. susi mounts them as capabilities, orchestrates a consensus **Swarm**, grounds every claim in **Evidence**, cross-examines with a **Truth** transformer, cryptographically **audits** actions, **sandboxes** untrusted code, compiles routine paths into **reflexes**, and **provisions** missing tools on the fly.

---

## Host contract

External clients can trust these ports — the daemon never silently drifts them:

| Port | Surface |
|------|---------|
| **9090** | GMCP / MCP HTTP (`/mcp`) |
| **9091** | GEMI HTTP (inference / models) |
| **9092** | A2A UDP discovery |
| **9093** | GMCP HTTP alias (streamable / SSE) |

- **`global susi`** — background daemon bound to the host substrate (`~/.susi`), not to a project folder.
- **`susi` CLI** — jailed to the caller's cwd; intents run against that workspace while the daemon owns ports, models, and lock state.
- **Canonical binary** — `~/.susi/bin/susi` (hot-reloads when the binary hash changes).

---

## The 8 pillars

1. **Swarm** — multi-agent consensus (`susi-gawd`) that routes, debates, and converges on intents.
2. **Evidence** — mission-scoped `EvidenceSession` ledger: real tool/MCP calls mint `ToolReceipt`s; answers cite receipt IDs — generated text cannot invent evidence.
3. **Truth** — `TruthTransformer` resolves citations from the live ledger and rejects narrative when citable receipts exist; file/workspace claims are re-checked against reality.
4. **Pluggable** — `CapabilityRegistry` mounts Candle / llama.cpp, vLLM / Ollama, OpenAI-compatible clouds, and MCP tools behind one interface.
5. **Audit** — agent actions signed into an immutable accountability chain.
6. **Sandbox** — Wasmer isolates untrusted plugins, agents, and reflexes.
7. **Reflexes** — routine intelligence distilled into fast Wasm / tensor paths.
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
```

Stdio MCP for editors that prefer a subprocess:
```bash
susi mcp
```

---

## Design principles

Defined in `.agents/`:
- **Autonomous by default** — plan and execute via swarm consensus, not step-by-step babysitting.
- **Grounded outputs** — claims checked against tools and workspace state.
- **Traceable reasoning** — thinking and tool calls are inspectable.
- **Concurrency-first** — Tokio, Rayon, Crossbeam, parking_lot on real hardware.

---

## License

Apache License 2.0. See [LICENSE](LICENSE). Provided as-is, without warranty; users run the engine and its agents at their own risk.
