# susi: The Zero-Trust AI Operating System

![SUSI Version](https://img.shields.io/badge/version-v0.3.0-blue.svg) ![License](https://img.shields.io/badge/license-Apache%202.0-green.svg)

> **susi** is a zero-trust AI operating system. It is infinitely **Pluggable (4)**—bring any model, any agent, and any MCP tool. We orchestrate them into a consensus-driven **Swarm (1)**. But unlike naive frameworks, susi operates on strict **Evidence (2)**. Our Universal **Truth (3)** Transformer cross-examines every claim to physically and semantically eliminate hallucinations. Every action is **Cryptographically Audited (5)** for compliance, untrusted code is tightly **Sandboxed (6)** for security, routine intelligence is compiled into fast **Reflexes (7)** for speed, and missing tools are **Autonomously Provisioned (8)** on the fly.

---

## The 8 Pillars of SUSI

1. **Swarm Orchestration**: A multi-agent consensus engine (`susi-gawd`) that routes, debates, and forces convergence on complex intents.
2. **Strict Evidence**: Structured `EvidenceRecord` and `Claim` types ensure no logic is accepted without a paper trail.
3. **Universal Truth**: The `TruthTransformer` evaluates claims against deterministic workspace realities and semantic cross-examinations, eliminating hallucinations.
4. **Infinitely Pluggable**: The dynamic `CapabilityRegistry` mounts local models (Candle, llama.cpp), remote APIs (vLLM, Ollama), and dynamic MCP tools behind a unified interface.
5. **Cryptographic Auditability**: Every agent action is cryptographically signed (`SHA256`) creating an immutable chain of accountability.
6. **Zero-Trust Sandboxing**: `wasmer`-powered Wasm environments securely isolate untrusted plugins, agents, and reflexes.
7. **Neural Reflexes**: Heavy LLM reasoning paths are distilled into lightning-fast, compiled Wasm and Tensor reflexes for routine tasks.
8. **Autonomous Provisioning**: Missing capabilities (like absent MCP servers) are actively scouted, downloaded, and hot-plugged at runtime without fatal crashes.

---

## Installation & Quickstart

### One-Liner (Linux / macOS / WSL)
```bash
curl -sSfL https://raw.githubusercontent.com/intellibitz/susi/main/install.sh | bash
```

The installer uses bash-specific features, so it must be piped into `bash`, not `sh` — on Debian, Ubuntu, and Alpine, `sh` is dash/ash, not bash, and will fail partway through.

Downloaded binaries and model weights are verified against a published SHA-256 checksum before use; if no matching binary/checksum is available for your platform, the installer falls back to building from source (requires Rust, see [rustup.rs](https://rustup.rs)).

By default the installer also registers a persistent background daemon (systemd user service on Linux, a launchd agent on macOS) that starts `susi` on login. Set `SUSI_NO_DAEMON=1` to skip this — note it goes on the `bash` side of the pipe, not before `curl` (a prefix before `curl` only sets the variable for `curl`, not for the piped-to shell):
```bash
curl -sSfL https://raw.githubusercontent.com/intellibitz/susi/main/install.sh | SUSI_NO_DAEMON=1 bash
```

### One-Liner (Windows PowerShell, native — not WSL)
```powershell
irm https://raw.githubusercontent.com/intellibitz/susi/main/install.ps1 | iex
```

### Usage
```bash
# Run a task across the agent swarm
susi "analyze this workspace and propose an optimization plan"

# Show build/identity info
susi identity

# Start local GEMI reasoning server (bound to localhost)
susi gemi

# Start the MCP server (stdio transport)
susi mcp
```

---

## Design principles

Defined in the `.agents/` workspace manifest:
- **Autonomous by default**: agents plan and execute without requiring step-by-step human approval, coordinating through swarm consensus.
- **Grounded outputs**: claims are checked against tool results and workspace state rather than taken on faith.
- **Traceable reasoning**: agent thinking and tool calls are logged for inspection.
- **Concurrency-first**: built on Tokio, Rayon, Crossbeam, and Parking_lot to use available hardware efficiently.

---

## License

Apache License 2.0. See [LICENSE](LICENSE). Provided as-is, without warranty; users run the engine and its agents at their own risk.
