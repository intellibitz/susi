# susi

![SUSI Version](https://img.shields.io/badge/version-v0.1.2023053-blue.svg) ![License](https://img.shields.io/badge/license-Apache%202.0-green.svg)

**susi** is a local-first Rust engine for running and orchestrating AI models. It loads model weights directly (`.safetensors`, GGUF, ONNX via Candle), queues work through a lock-free, async intent pipeline, and can fan work out across multiple agents running in parallel. It exposes tools over the Model Context Protocol (MCP) and checks its own outputs against the actual state of the workspace before treating them as fact.

---

## What it does

1. **Model execution**: Runs local model weights across multiple formats (`.safetensors`, GGUF, ONNX) via Candle, with hardware-aware memory management.
2. **Concurrent task queue**: Non-blocking intent ingestion via `crossbeam::queue::SegQueue`, with parallel execution across agents using `rayon`.
3. **MCP tool registry**: JSON-RPC 2.0 tool server (`gmcp`) for dynamic tool discovery, workspace file I/O, and process execution.
4. **Output verification**: Structured `EvidenceRecord`/`Claim` types and a `TruthTransformer` step that checks generated claims against actual workspace state before surfacing them.

---

## Installation & Quickstart

### Universal One-Liner (Linux / macOS / Windows / WSL)
```bash
curl -sSfL https://raw.githubusercontent.com/intellibitz/susi/main/install.sh | sh
```

Downloaded binaries and model weights are verified against a published SHA-256 checksum before use; if no matching binary/checksum is available for your platform, the installer falls back to building from source (requires Rust, see [rustup.rs](https://rustup.rs)).

By default the installer also registers a persistent background daemon (systemd user service on Linux, a launchd agent on macOS) that starts `susi` on login. Set `SUSI_NO_DAEMON=1` before running the installer to skip this:
```bash
SUSI_NO_DAEMON=1 curl -sSfL https://raw.githubusercontent.com/intellibitz/susi/main/install.sh | sh
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
