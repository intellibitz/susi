# SUSI Operational Workflow

* **Current Engine Version**: `v0.1.2022916`

This document defines the self-contained Meta-Workflow for substrate evolution and the Federated Parallelism logic of the active `susi` engine under the **Continuous Intent Manifold** architecture, governed by the high-tier Agent of Agents (AoA) and Swarm Master Authority (SMA).

## 1. Continuous Intent Manifold Ingestion

1. **Pulse Ingestion**: SUSI accepts natural language intents (Pulses) via a non-blocking asynchronous listener and stages them in the lock-free `SubstratePulseQueue` (`crossbeam::queue::SegQueue`).
2. **Manifold Analysis**: Every intent is evaluated by `IntentManifold::analyze` under high-tier SMA oversight to dynamically determine:
    - **Scope of Impact**: `Read` (ephemeral lookup) ➔ `Write` (workspace I/O) ➔ `Mutate` (substrate administration) ➔ `SelfExtend` (reflex synthesis / autonomous code evolution).
    - **Risk Profile**: `Low` ➔ `Medium` ➔ `High` ➔ `Critical`.
3. **Dynamic Execution Graph (DAG)**: The AoA orchestrator dynamically constructs a tailored execution graph where security, capability, and verification gates adapt fluidly to the intent's actual requirements within the self-contained ecosystem.

## 2. Universal Swarm Execution (Mandatory)

4. **The Swarm Mandate**: All non-read operations initialize a specialized GAWD Swarm at maximum hardware capacity utilizing Rayon work-stealing parallelism, fully authorized by the SMA.
5. **Phase A: Swarm-Driven Kernel Bootloader**: Upon startup, `SubstrateKernelLoader::boot_kernel` interrogates host hardware, tests port endpoints (GMCP, GEMI, UDP), and deploys the active SUSI Swarm to dynamically assemble and hot-plug core substrate modules (`gawd-swarm`, `gmcp-protocol`, `gemi-inference`, `truth-transformer`).
6. **Phase B: Evidence IR & Verification**: Agent outputs are structured into `EvidenceRecord`, `Claim`, and `EvidenceSource` records, providing machine-verifiable provenance before ingestion by the GEMI reasoning engine and `TruthTransformer` physical workspace verification.
7. **Phase C: ReAct Protocol Output**: Swarm telemetry is formatted into structured ReAct JSON objects (`action`, `action_input`, `observation`, `thought`) followed by clean Markdown results, ensuring strict protocol compliance for external clients.
