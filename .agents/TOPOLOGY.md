# SUSI Substrate Topology & Execution Logic

* **Current Engine Version**: `v0.1.2022918`

This document defines the structural native components, orchestrated meta-layers, and execution workflow of the `susi` substrate under the **Continuous Intent Manifold** architecture. The topology forms a fully self-contained, closed-loop ecosystem.

## 1. Agent of Agents (AoA) - Supreme Coordination & Governance
1. **GAWD / SMA (Swarm Master Authority)**: High-tier universal swarm supervisor, absolute governance authority, and multi-agent parallel dispatcher. (Tier: 1)
2. **SusiAdmin**: Native administrative substrate for release orchestration, compliance auditing, and axiomatic pulse ingestion under SMA direction. (Tier: 1)
3. **SusiRuntimeAdmin**: Substrate maintenance authority (Hardware audit, Model provisioning & peak selection). (Tier: 1)
4. **EvolutionManager**: Substrate self-healing and autonomous Motion Rule execution. (Tier: 1)
5. **SusiDaemon**: Persistent background host and process manager for the GMCP/GEMI server fleet. (Tier: 1)
6. **SubstrateKernelLoader**: Swarm-driven dynamic module bootloader and port endpoint verification engine. (Tier: 1)

## 2. Agents - Functional Execution Units
7. **SusiRuntimeAgent**: Autonomous environment preparation agent (Weights & Tools). (Tier: 1)
8. **HardwareAgent**: Autonomous hardware interrogation and compute resource saturation agent. (Tier: 1)
9. **SafetyAgent**: Governance auditor and destructive command interceptor. (Tier: 1)
10. **SecurityAgent**: Credential masking and exfiltration prevention agent. (Tier: 1)
11. **EvolutionAgent**: Autonomous drift detection and self-healing agent. (Tier: 1)
12. **GmcpAgent**: MCP JSON-RPC 2.0 endpoint verification agent. (Tier: 1)
13. **LibraryScoutAgent**: Live crates.io REST API package scouting agent. (Tier: 1)
14. **ContextAgent**: High-density context manager and workspace analyzer. (Tier: 1)
15. **NeuralAgentFactory**: Autonomous synthesis and recruitment of domain-specific specialist agents. (Tier: 1)

## 3. Engines - Execution & Inference Substrates
16. **SUSI-Alpha**: Microsecond intent classification and deterministic neural reflex engine. (Tier: 0)
17. **ReflexSynthesizer**: Native Rust code distillation and reflex generation for distilled intents. (Tier: 0)
18. **UniversalExecutionSubstrate**: Absolute engine-agnosticism. SUSI can execute any model in the world using a unified, hardware-saturated inference layer. (Tier: 2)
19. **GEMI**: Deep reasoning bridge and unified cloud provider inference racing. (Tier: 2)
20. **SUSI-Vision**: Hardware-saturated neural vision substrate for visual/text semantic fusion. (Tier: 2)
21. **SUSI-Audio**: Hardware-saturated neural audio substrate for spectral logic distillation. (Tier: 2)

## 4. Models - Neural Intelligence & Weights
22. **NativeAlphaModel**: Local neural weights (`susi-alpha.safetensors`) for deterministic reflex. (Tier: 0)
23. **NativeReasoningModel**: Distilled Tier 2 logic weights (`susi-reason.safetensors`) trained on the SUSI genome. (Tier: 2)
24. **UniversalSubstrateModels**: Absolute model-agnosticism. SUSI can ingest any model weights (GGUF, Safetensors, ONNX, PyTorch) from any global repository with full SHA-256 integrity verification. (Tier: 2)

## 5. MCPs - Interoperability & Tooling (GMCP Infrastructure)
25. **GMCP Server**: Background daemon exposing multi-protocol endpoints (RPC: 9090, HTTP/SSE: 9093, UDP: 9092). (Tier: 1)
26. **GMCP Host**: The `susi` CLI proxy that acts as a protocol bridge within the autonomous ecosystem. (Tier: 1)
27. **GEMI Server**: Dedicated RESTful endpoint (Port 44075) for Tier 2 reasoning and model management. (Tier: 1)
28. **MetaMcpServer**: External Model Context Protocol servers connected via stdio or TCP. (Tier: 1)
29. **Evidence IR Substrate**: Structured `EvidenceRecord`, `Claim`, and `EvidenceSource` provenance pipeline. (Tier: 1)

## 6. Realized Architectural Capabilities
30. **Unified Multi-Modal Embedding Space**: 1024-dimensional neural projection space where text, vision, and audio intents are unified.
31. **Autonomous Self-Validation**: Continuous self-validation tests on local CPU/GPU/RAM substrates to verify system health.
32. **Unified Interaction Protocol**: Every swarm intent or system request is anchored in a verifiable typed test entry.
33. **The Hardware-Only Limit Principle**: Physical hardware capacity is the sole and final limit on performance and intelligence.
34. **Optimal Asynchronous Orchestration**: 100% non-blocking I/O and mandatory multi-threaded execution for all internal and external requests.
35. **Universal Swarm Operation**: 100% of substrate operations across all tiers execute through the multi-threaded GAWD Swarm.
36. **Lock-Free Native Substrate**: Elimination of blocking thread locks from the execution critical path.
37. **<2ms Ultra-Reflex Substrate**: 100% of engine internal operations complete in under 2ms.
38. **Recursive Swarm Parallelism**: Autonomous decomposition of complex missions into independent sub-tasks executed in parallel.
39. **Universal Non-Blocking & Decoupled Concurrency Substrate**: Codified use of SOTA crates including `tokio`, `rayon`, `parking_lot`, `crossbeam`, `flume`, and `tracing`.
40. **Industry-Standard MCP Interop Bus**: Native JSON-RPC 2.0 transport multiplexing and full MCP server/client proxy compliance (via `rmcp` SDK).
41. **Autonomous Substrate Administration**: Deployment of the `SusiRuntimeAdmin` for optimal execution environment maintenance.
42. **Neural Agent Synthesis**: Dynamic synthesis protocol for specialist agents via `NeuralAgentFactory`.
43. **Federated Experience Aggregation**: Secure protocol for distilling reasoning experience into a centralized Knowledge Vault.
44. **SOTA Library Scouting Protocol**: Autonomous discovery and benchmarking of high-performing Rust crates (via `LibraryScoutAgent`).
45. **Glass Box Transparency & Omni-Trace Reasoning**: 100% visibility into atomic reasoning traces and substrate operations.
46. **Universal Async Pulse Pipeline**: Decoupled, asynchronous intent ingestion for zero-stall execution.
47. **Absolute Accountability Substrate**: 100% unique identification and attribution for every atomic action.
48. **Operational Coding Intelligence**: Functional AST parsing, Tantivy code search, and Bollard Docker isolation.
49. **Operational Research Intelligence**: Functional Headless Chrome automation and Neural RAG.

## 7. Operational Workflow & Intent Manifold

### 7.1 Continuous Intent Manifold Ingestion
1. **Pulse Ingestion**: SUSI accepts natural language intents (Pulses) via a non-blocking asynchronous listener and stages them in the lock-free `SubstratePulseQueue` (`crossbeam::queue::SegQueue`).
2. **Manifold Analysis**: Every intent is evaluated by `IntentManifold::analyze` under high-tier SMA oversight to dynamically determine:
    - **Scope of Impact**: `Read` (ephemeral lookup) ➔ `Write` (workspace I/O) ➔ `Mutate` (substrate administration) ➔ `SelfExtend` (reflex synthesis / autonomous code evolution).
    - **Risk Profile**: `Low` ➔ `Medium` ➔ `High` ➔ `Critical`.
3. **Dynamic Execution Graph (DAG)**: The AoA orchestrator dynamically constructs a tailored execution graph where security, capability, and verification gates adapt fluidly to the intent's actual requirements within the self-contained ecosystem.

### 7.2 Universal Swarm Execution (Mandatory)
4. **The Swarm Mandate**: All non-read operations initialize a specialized GAWD Swarm at maximum hardware capacity utilizing Rayon work-stealing parallelism, fully authorized by the SMA.
5. **Phase A: Swarm-Driven Kernel Bootloader**: Upon startup, `SubstrateKernelLoader::boot_kernel` interrogates host hardware, tests port endpoints (GMCP, GEMI, UDP), and deploys the active SUSI Swarm to dynamically assemble and hot-plug core substrate modules (`gawd-swarm`, `gmcp-protocol`, `gemi-inference`, `truth-transformer`).
6. **Phase B: Evidence IR & Verification**: Agent outputs are structured into `EvidenceRecord`, `Claim`, and `EvidenceSource` records, providing machine-verifiable provenance before ingestion by the GEMI reasoning engine and `TruthTransformer` physical workspace verification.
7. **Phase C: ReAct Protocol Output**: Swarm telemetry is formatted into structured ReAct JSON objects (`action`, `action_input`, `observation`, `thought`) followed by clean Markdown results, ensuring strict protocol compliance for external clients.
