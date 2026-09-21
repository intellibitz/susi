---
schema = "susi/roadmap/v1"
version = "0.7.0"
strategic_priority = "Federation"
target_version = "0.2.0"
---

# SUSI Substrate Roadmap: The Sovereign Will


This document defines the structural roadmap and future evolution goals of the `susi` substrate.

---

| ID | Type | Evolutionary Vector | Mastery Target | Progress (verified 2026-09-17) |
| :--- | :--- | :--- | :--- | :--- |
| VC-200-001 | [SWARM] | Federated Cross-Node Consensus | P2P Non-blocking Swarm | PARTIAL: UDP LAN discovery, TCP JSON-RPC peer dispatch, and trust-score ranking already worked. Closed the routing gap the code itself flagged ("bloom-filters of peer registries for perfect routing"): peers now exchange a real 256-bit `CapabilityBloom` of their registered tool names during ping/pong, and `rank_peers_for_goal` scores peers by goal/capability overlap instead of trust alone (`src/gawd/amas.rs`). Remaining gap: consensus is still local-blackboard-weighted, not a true distributed agreement protocol (no Raft/Paxos-style commit). |
| VC-200-002 | [CORE] | Neural Self-Repair & Hot-Reload | Autonomous Trait Patching | PARTIAL, reinterpreted: literal runtime Rust trait patching isn't achievable in safe Rust. Delivered the real equivalent — `resolve_capability_gap` now falls back to `ReflexSynthesizer::synthesize_wasm_reflex`, which generates and compiles an input-dependent WASI reflex and hot-loads it via the existing `reflex_<name>` convention with **no daemon restart** (`src/gawd/reflex_synth.rs`, `src/gmcp/tools/mod.rs`). Also fixed two latent bugs found while building this: the target triple (`wasm32-wasi`, removed from rustc) was corrected to `wasm32-wasip1`, and the synthesized source previously had no `fn main()`/`_start`, so it could never have executed even if it had compiled. Compilation requires the `wasm32-wasip1` rustup target, **not installed in this dev environment** — verified end-to-end via source-validity + FFI tests; live wasm compile/execute is covered by a test that skips honestly (not silently) when the target is absent. |
| VC-200-003 | [FUSION] | Spectral Audio-Visual Fusion | 1024-D Multi-Signal Manifold | PARTIAL, bug fixed: `SusiUnifiedSubstrate::project_to_unified_space` (`src/gemi/unified.rs`) claimed to be a "1024-dimensional neural projection" but only hashed image/audio **path string lengths** — it never touched pixel or waveform data, and its own comment admitted "Real logic would involve loading vision/audio encoders." Fixed to call the real `SusiVisionEngine`/`SusiAudioEngine` feature extractors and fuse their actual output into non-overlapping subspaces: vision `[0,512)`, audio `[512,768)`, text `[768,1024)`. Remaining gap: per-modality encoders are untrained (randomly initialized linear projections, consistent with the rest of this substrate's Tier 2 "distillation" stubs) and there is no cross-modal attention — this is real fusion of real features, not a trained multimodal model. |
