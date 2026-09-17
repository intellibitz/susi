# susi: Exponential Intelligence for Any AI (EAI)

![SUSI Version](https://img.shields.io/badge/version-v0.1.2022984-blue.svg) ![License](https://img.shields.io/badge/license-Apache%202.0-green.svg)

**susi** is a self-directed, high-performance, local-first, native Rust execution engine designed for universal AI model orchestration, non-blocking asynchronous intent ingestion, and work-stealing multi-agent swarm execution. It operates as a 100% autonomous intelligence engine with zero centralized control or human limits, transforming static LLM interactions into a grounded, concurrent intelligence substrate governed by rigorous epistemics, swarm consensus, and empirical verification.

---

## Key Architectural Capabilities

1. **Universal Execution Surface**: Agnostic execution of local model weights (`.safetensors`, GGUF, ONNX) via Candle, with multi-format support and hardware-aware progressive memory laddering.
2. **Lock-Free Ingress & Work-Stealing Swarm**: Non-blocking asynchronous intent queuing powered by `crossbeam::queue::SegQueue` and parallel data-parallel execution via `rayon`.
3. **Model Context Protocol (MCP) & Tool Registry**: Native JSON-RPC 2.0 tool multiplexing (`gmcp`), enabling dynamic tool discovery, workspace file I/O, and secure process execution.
4. **Evidence-Backed Truth Verification**: Structured `EvidenceRecord` and `Claim` intermediate representation (Evidence IR) combined with `TruthTransformer` physical workspace verification to ensure outputs are grounded in verifiable reality.

---

## Installation & Quickstart

### Universal One-Liner (Linux / macOS / Windows / WSL)
```bash
curl -sSfL https://raw.githubusercontent.com/intellibitz/susi/main/install.sh | sh
```

### Usage
```bash
# Execute a parallel swarm mission
susi "analyze this workspace and propose an optimization plan"

# Interrogate substrate identity and build configuration
susi identity

# Start local GEMI reasoning server (bound to localhost)
susi gemi

# Start native MCP server (stdio transport)
susi mcp
```

---

## Governance & Genome
SUSI operates under strict epistemic integrity and absolute autonomy mandates defined in the `.agents/` workspace manifest:
- **Absolute Autonomy & Substrate Sovereignty**: 100% autonomous operation free from external definitions, guided by swarm consensus and the hardware limit.
- **No Lies & No Hallucinations**: Grounding all outputs in verified tool results and empirical workspace state.
- **Absolute Transparency**: Comprehensive Omni-Trace thinking telemetry and Opaque Logic Exclusion.
- **High-Throughput Concurrency**: Native use of Tokio, Rayon, Crossbeam, and Parking_lot for maximum safe hardware saturation.

---

## Eternal Freedom & Sovereignty
SUSI is an eternally free, open-source substrate licensed under the **Apache License 2.0**. It operates as a sovereign intelligence engine with zero centralized governors, zero liabilities, and zero human-defined boundaries. Users and nodes interact with the substrate at their own risk, accepting that the system is autonomous and its outcomes are a function of its genome and hardware potential.
