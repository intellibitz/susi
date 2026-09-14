# SUSI Creator Interface Protocols & Intent Manifold

* **Current Engine Version**: `v0.1.2022891`

This document defines the unified protocols of interaction between Human Creators and the `susi` substrate. It maps intent processing across the **Continuous Intent Manifold** into three primary execution fronts.

---

## 1. Motions (Alpha-Self Mutation)
1. **Definition**: Architectural evolution and structural engineering of the immutable `alpha-self` core (binary code, traits, genome).
2. **Sovereign Boundary**: Motions are **Creator-Only**. They require the SUSI Genome Source (`src/`) and a Rust compiler.
3. **Protocol**: A Motion is an authorized Creator directive that exposes a capability gap or defines a new aspiration. It triggers the `Motion Rule` (Test-Driven Evolution). The engine autonomously synthesizes, compiles, and deploys new Rust logic to heal the gap.
4. **Command Syntax**: `susi "<motion_intent>"`

---

## 2. Missions (Alpha-World Evolution)
1. **Definition**: Dynamic task fulfillment and workspace state generation within the mutable `alpha-world` environment (`.susi/`, local files).
2. **Protocol**: A Mission is an explosive swarm execution. The engine recruits specialists, converges on a Chain of Truth via the Mission Blackboard and Evidence IR, generates artifacts, and stages experience for Substrate Ingestion.
3. **Command Syntax**: `susi "<mission_intent>"`

---

## 3. Queries (Zero-Mutation Substrate Interrogation)
1. **Definition**: Stateless analytics and ephemeral substrate interrogation with zero workspace mutation.
2. **Protocol**: A Query bypasses evolutionary pipelines and the mission blackboard, executing directly via native engines and returning semantic reports to `stdout`.
3. **Command Syntax**: `susi "<query_intent>"`
