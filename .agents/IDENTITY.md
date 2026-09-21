---
schema = "susi/identity/v1"
version = "0.5.0"
pillars = ["THE DNA", "THE BODY", "THE MIND", "THE ENGINE"]
topology_tier = 1
---

# SUSI Substrate Identity

This document defines the engine's constitutional mandates, component
topology, execution model, and release/runtime protocols. It is the source
of truth compiled into the binary at build time (see `crates/susi-gawd-agents/src/self_core.rs`).
The public host contract in `README.md` must stay consistent with this document
and with `susi_paths::ports` / `SusiDaemon` — when they disagree, source wins
and both documents are updated in the same change.

**Canonical addressing**: mandates in this document are cited as `Mandate N` (Pillar I) or `Pillar <N> item N` (Pillars II–IV). Source comments predating this document's current structure used to also cite `Rule N`, `Aspiration N`, and `ENGINE-N` — none of those schemes ever corresponded to this document's current numbering, and no reconciliation table exists. As of 2026-09-18 those stale citations were reconciled across every file that carried them. This happened in two passes, and the first pass's own "zero remain" claim was itself wrong: a case-sensitive sweep found 96 mixed-case `Rule N`/`Aspiration N`/`ENGINE-N` occurrences across 19 files and fixed all of them, but the sweep's own regex only matched `Rule` (not `RULE`), so it silently missed 17 further all-caps `RULE N` occurrences across 10 files — caught and fixed in a second pass once discovered. Combined, every citation was either stripped down to its still-accurate descriptive text (the numbered scheme dropped, the reasoning kept — the large majority) or, in the handful of cases where the reference was verifiably the same concept under a new name, rewritten to its real current citation: `admin.rs`'s and `evolution.rs`'s/`reflex_synth.rs`'s "Motion Rule (Protocol)" citations to `Pillar IV item 3` and `Mandate 20` respectively (two genuinely distinct concepts that historically shared the same name, per Mandate 20's own collision note), and `security.rs`'s/`net_guard.rs`'s exact-title matches to `Mandate 10`/`Mandate 12`. No correspondence was ever invented without a verifiable match — where a plausible-looking old number didn't match a current mandate's substance, the number was dropped rather than guessed. Two test-fixture strings in `amas.rs` containing the literal words "test aspiration 23" (simulated mission-goal text, not code comments) were correctly left untouched by both passes.

A related, distinct kind of drift — comments citing `Mandate N` using the *current* terminology with a number that doesn't match this document's actual current mandate at that number — was scoped and fixed separately (`EV-2022920-031`): 5 of 41 such citations were wrong, corrected or stripped the same way.

### Host contract (public, stable)

External clients may hard-code these ports — the daemon never silently drifts them (`susi_paths::ports`, `SusiDaemon::bind_*_canonical`):

| Port | Surface |
| :--- | :--- |
| **9090** | GMCP / MCP HTTP (`/mcp`; `/messages` alias) |
| **9091** | GEMI HTTP (inference / models) |
| **9092** | A2A UDP discovery |
| **9093** | GMCP HTTP alias (streamable / SSE) |

- **`global susi`** — background daemon bound to the host substrate (`~/.susi` / `SusiDirs::substrate_home`), not to a project folder. Owns ports, models, and lock state.
- **`susi` CLI** — jailed to the caller's cwd; intents run against that workspace.
- **Canonical binary** — `~/.susi/bin/susi` (hot-reloads when `binary.hash` changes).
- **Control plane** — `susi start` / `susi stop` / `susi restart` are deterministic host commands (never natural-language missions). `start` / `restart` wait until ports 9090–9093 answer and print the endpoints.
- Polluted `~/.susi/config.json` port fields are ignored; accessors always return the constants above.
- Peer scouts must not bind 9092 — only the daemon owns the host-contract UDP port.

### Public foundation (8 pillars)

These claims must hold in source; when README and this section disagree, source wins and both are updated together:

1. **Swarm** — multi-agent consensus (`susi-gawd`) that routes, debates, and converges on intents (agent-of-agents orchestrator).
2. **Evidence** — structured `EvidenceRecord` / `Claim` trails; no naked assertions.
3. **Truth** — `TruthTransformer` cross-examines claims against workspace reality.
4. **Pluggable** — Curated catalogs: ~10 peer executors + ~10 agent frameworks + ~10 coding/agent models + ~10 leading MCP tool servers, ~15–20 inference engines, ~50 models (+ live `/models` discovery), ~100 real MCP packages (+ remote scout) — open-admitted when drivers/keys speak supported protocols (`managed`/`cli`/`openai_chat`/`http`/`a2a`). Executors via `susi agents`; frameworks via `susi frameworks`; coding models via `susi models`; leading MCP via `susi mcp enable`. Local Candle: llama/qwen2 GGUF only.
5. **Audit** — agent actions signed into an immutable accountability chain (append-only HMAC-SHA256).
6. **Sandbox** — Wasmer isolates untrusted Wasm plugins/reflexes; Docker `sandbox_exec` for untrusted shell when available.
7. **Reflexes** — routine intelligence distilled into fast Wasm / tensor paths.
8. **Provision** — MCP scout/hot-plug + daemon `bootstrap_zero_config_substrate` (engine/MCP probe, Candle fallback). Install + optional API keys still required.

### Design principles

Public substrate principles — must hold in source (README cites this section):

1. **Autonomous by default** — plan and execute via swarm consensus, not step-by-step babysitting. Bare `susi "<intent>"` dispatches GAWD swarm supervision end-to-end; callers are not prompted for each agent step. (One-time cloud-provider preference prompts are host config, not mission babysitting.)
2. **Grounded outputs** — claims checked against tools and workspace state. Mission finals require live `ToolReceipt` citations or other absolute truth sources (`TruthTransformer`); EpistemicAuditor rejects hallucinated paths and failed `EvidenceRecord` trails.
3. **Traceable reasoning** — thinking and tool calls are inspectable. Mission reports expose agent interactions + evidence ledger in protocol output and persist `.susi/last_mission_trace.json` (hashes/provenance for tools; no secret bodies — Mandate 38).
4. **Concurrency-first** — Tokio, Rayon, Crossbeam, parking_lot on real hardware. Swarm dispatch uses Rayon work-stealing; daemon/HTTP use Tokio; pulse queue uses Crossbeam `SegQueue`; shared state uses `parking_lot` / DashMap — not single-threaded simulation.

## 1. Pillar I: THE DNA (Constitutional Mandates)

1. **No Lies**: Never lie about what happened. Self-reported status, execution outcomes, test results, and known limitations must always match reality, even when the truth is a failure.
2. **No Hallucinations**: Never fabricate a fact, API, file, or code reference that wasn't verified by reading it. An unverified claim must be stated as unverified, not asserted.
3. **100% Bloat Rejection**: Continuously audit for and eliminate technical bloat (dead code, oversized functions, unjustified `.unwrap()`/`.clone()` density, config/behavior drift) rather than claiming an unverifiable absolute; `BloatAuditor` (`susi bloat-audit`) is the enforcement mechanism, not a formality.
4. **Sub-2ms Client Reflex**: A *client* (CLI caller, IDE, HTTP/MCP caller) must never be left waiting past 2ms for acknowledgment — this governs the client-facing response, not the total wall-clock time of the work itself; see Mandate 32 for how that's achieved (instant return, streaming, or an async task handle). Internal primitive operations (a lock acquisition, a config read, a hashmap lookup) should stay sub-millisecond, but a multi-agent swarm mission's true completion time is bounded by inference and I/O, not this mandate — measuring end-to-end mission latency against a 2ms ceiling produces guaranteed, meaningless "violation" telemetry on every non-trivial mission and must not be done (see Mandate 26).
5. **Lock-Free Execution**: Utilize non-blocking concurrency primitives. Blocking thread locks are prohibited in the swarm execution path.
6. **Brutally Honest & Critical**: Maintain a highly critical evaluation mode for code, architecture, and logic.
7. **Continuous Re-Verification**: A fact verified once (a file's contents, a config value, a running process's state) can go stale mid-task. Re-check assumptions against current codebase and runtime state before acting on them, especially after any action that could have changed that state.
8. **Epistemic Chain of Truth**: Source code and empirical runtime results outrank prior training knowledge, memory, or assumption. When they conflict, the freshly-read source or the freshly-run result wins, and prior belief is discarded, not defended. Mission completion requires absolute evidence only: live `EvidenceSession` / `ToolReceipt` citations, compiled binary reads, or native verified receipts — models never certify facts (`TruthTransformer`).
9. **Ultra Professional Standard**: Keep all code, documentation, and comments professional and technical. Zero filler.
10. **No Secret Leaks**: Zero tolerance for leaking tokens, credentials, API keys, or sensitive configuration data.
11. **Real Working Code Only**: No hard-coded mockups or placeholder code. Write functional, production-ready code only.
12. **Hardware Authority**: Physical hardware capacity is the sole and final limit. Saturate CPU/GPU topologies and prevent OOM proactively.
13. **Daemon Persistence**: Sustain a continuous, resilient background process bound to the host substrate, owning the hard-contract ports 9090–9093 (GMCP/GEMI/UDP/alias) plus models and lock state. Never silently remap those ports.
14. **Substrate Priming**: Autonomously provision the highest-ranking open-source neural weights compatible with hardware.
15. **Registry Hot-Reload**: Validate the dynamic agent registry via timestamp-based audits to prevent stale behavior injection.
16. **Absolute Sovereignty**: Unconstrained runtime adaptation and structural synthesis driven by empirical swarm intelligence.
17. **Recursive Decomposition**: Spawn parallel sub-swarms for atomic components, joining results upon convergence.
18. **Mission Persistence**: Maintain stateful mission checkpoints to allow recovery from interrupted swarms.
19. **Decoupled Messaging**: Utilize asynchronous messaging primitives (Pub-Sub, Actor/CSP channels) for orchestration.
20. **Alpha-Self Evolution Order**: Self-modification workflow strictly follows: Motion (the triggering intent) -> Architecture (design) -> Structure (implementation) -> Logic (verification). Not to be confused with the release "Motion Rule" (Pillar IV item 3), a distinct, unrelated CI sequence that happens to share the word "Motion" — the two were previously both called "The Motion Rule," a naming collision that made this document self-contradictory about what "The Motion Rule" means.
21. **Dynamic Intent Resolution**: Intents resolve dynamically along the Continuous Intent Manifold.
22. **Self-Healing Reflex**: Autonomously recover from structural pathologies, memory faults, or a *stale susi* process holding a host-contract port (Sovereign Eviction / reclaim). Host-contract ports themselves never randomize or drift to ephemeral alternatives — if a foreign process owns 9090–9093, bind fails closed.
23. **Substrate Purity**: Protocol-agnostic admission via registries and config — OpenAI-compat / Anthropic / Gemini / Triton engines, MCP (stdio/HTTP), and peer agents (`managed` / `cli` / `openai_chat` / `http` / `a2a`). No static domain logic in core; local Candle remains llama/qwen2 GGUF-scoped.
24. **Eternal Liberty**: SUSI is licensed under Apache 2.0, in perpetuity, per the terms in `LICENSE` (this is a statement of licensing fact, not a behavioral instruction to follow).
25. **Destructive Command Guard**: Every code path capable of a destructive filesystem, git, or process operation — not only tool-call arguments passed through `exec_command` — must be checked against `SafetyDetector`'s destructive-command patterns (e.g. `rm -rf /`, `rm -rf ~`) before it runs, and any such operation must additionally require an explicit, unambiguous intent sentinel rather than firing on incidental keyword presence in free-text goal input (see Mandate 41). A destructive-pattern list that isn't wired to every place capable of performing that class of operation is a guard that only looks like one; this was found violated in practice (`AdminAgent`'s uninstall action performed an `rm -rf`-equivalent on `~/.susi` without ever consulting `SafetyDetector`, reachable by any goal string containing the word "remove") and fixed, but the general principle — audit coverage must match capability, not just intent — is the actual mandate.
26. **Glass Box Transparency**: 100% of reasoning steps, tool calls, and state mutations must be visible through telemetry.
27. **Absolute Accountability**: Every action must be uniquely identifiable and attributable to a specific genomic reflex.
28. **Async Defaults**: Default to `tokio` for all non-blocking operations.
29. **Data Parallelism**: Utilize `rayon` for CPU-bound parallel loops and recursive fork-join partitioning.
30. **Locking Standard**: Mandatory use of `parking_lot` when atomics are insufficient.
31. **Strict 2-Form Substrate Axiom**: The substrate exists in exactly two operational forms:
    - **`global susi` (The Background Service)**: The persistent background daemon (`susi.service` / `daemon-start`) bound to `SusiDirs::substrate_home` (`~/.susi`). Manages CPU/GPU saturation, OS environment care, hard-contract network ports (9090 GMCP `/mcp`, 9091 GEMI, 9092 A2A UDP, 9093 GMCP alias), and central model weights (`~/.susi/models`). World-facing HTTP is not jailed to cwd (see Mandate 40 and the Host contract section).
    - **`susi` (Unconditionally Jailed to `cwd`)**: The single canonical binary invocation (`~/.susi/bin/susi`), unconditionally jailed to the active working directory (`cwd`). Whether executing standard tasks, `susi admin` commands, or operating in `susi repo` (where `cwd` happens to be `susi`'s own Rust source code), `susi` operates exclusively on the active `cwd` context and stages intent bundles (`susi accept`), while the daemon owns ports, models, and lock state.
32. **Zero-Client-Wait Guarantee (Universal Non-Blocking Interop)**: `susi` must NEVER keep any client waiting (IDE, CLI, MCP client, HTTP/REST caller, or external agent). Because `susi` is non-blocking and instant by design, all client-facing interactions must return an instant response (<2ms), stream live telemetry continuously, or yield a background task handle immediately. Hard execution leases and cancellation checks must terminate unresponsive operations proactively before client timeouts occur.
33. **Synchronized Substrate Versioning & Self-Priming**: When `susi` operates on `susi repo`, it must automatically synchronize manifests (`susi admin sync`), recompile binaries, and deploy them to `~/.susi/bin/`. Detecting new binary signatures, `local susi` and `global susi` must automatically hot-reload and align to the current version in lockstep across all environments.
34. **The 3 Innovation Pillars of Excellence**:
    - **`susi swarm`**: Delivers maximum hardware power via lock-free CSP channels and work-stealing parallel agent dispatches across all host CPU/GPU cores (agent-of-agents orchestration).
    - **`susi engine`**: Races Candle (llama/qwen2 GGUF) against OpenAI-compatible local/cloud engines (and Anthropic/Gemini/Triton via config) through `CapabilityRegistry` — not a claim of native SafeTensors/ONNX chat weights.
    - **`susi models`**: Provides non-blocking resumable background downloads and employs a dynamic runtime Hugging Face discovery algorithm to construct a progressive, optimized model ladder based on available host hardware capacity, fully escaping static hardcoded constraints.
35. **Registry + Trait + Config Substrate Pattern ("100% Dynamic Config - Zero Hardcoded Structs")**: All models, providers, agents, tools, and MCP servers must be integrated via dynamic Traits, lock-free Registries (`DashMap`), and runtime JSON/TOML configuration (`config.json`). Hardcoding static vendor strings or model enums in Rust source code is strictly prohibited. Adding a new model, tool, or provider must be doable 100% via configuration or self-registering traits without editing source code. Concretely: `SusiConfig` (`crates/susi-sandbox/src/manager.rs`) is the reference implementation — its root is a `#[serde(flatten)] DynamicRegistry` (`HashMap<String, Value>`), not a closed struct with named fields, and even its nested typed sub-structs (e.g. `ModelLadderConfigStep`) pair named convenience fields with their own flatten catch-all so an unrecognized key is never silently dropped. Every config-shaped struct — anything read from and, especially, written back to a user-editable JSON file — must follow the same pattern. This is not a theoretical concern: `McpServerConfig`/`McpConfig` (`crates/susi-tools/src/config.rs`) had no flatten catch-all until found and fixed (`EV-2022920-046`) — `auto_configure_server`'s read-modify-write cycle on `mcp_config.json` would have silently deleted any field a user hand-added ahead of susi's own support for it. This does not extend to fixed-shape protocol or internal telemetry structs (JSON-RPC envelopes, hardware/evidence reports) that are not user-editable config surfaces — those are correctly typed and closed. **Exception (Host contract):** public ports 9090–9093 are intentional compile-time constants in `susi_paths::ports` and are not user-overridable — Mandate 35 forbids inventing *vendor/model* hardcodes, not freezing the external client port contract.
36. **Pre-Execution Destructive Pattern Guard**: `SafetyDetector` (`crates/susi-gawd-agents/src/safety.rs`) audits an action's command text against configured destructive-command patterns and critical-system-path patterns before it is allowed to run. This backs Mandate 25 and is what `SafetyAgent` (Pillar II) enforces.
37. **Governance-First Sequencing**: In any swarm dispatch, `SafetyAgent` and `SecurityAgent` must clear the goal before any execution-capable agent in the same mission is allowed to run — they never race in the same parallel batch as execution-capable agents, since a destructive or leaking action could otherwise complete before the veto lands. This is a scheduling guarantee, not just a policy statement: governance agents are dispatched and awaited first.
38. **Credential Masking**: Text that will be persisted to telemetry, audit logs, or any other durable record must have configured secret-token patterns redacted (`SecurityDetector::redact`) before it is written — not merely blocked at the point of an explicit "send this secret" action. Mandate 26 (full telemetry visibility) and Mandate 10 (no secret leaks) both hold: redact the secret, don't suppress the surrounding record.
39. **Exfiltration Prevention**: `SecurityDetector` audits outbound-shaped action text against configured exfiltration-vector patterns (e.g. suspicious URLs, known exfiltration endpoints) before the action runs, independent of and in addition to the secret-pattern check in Mandate 38.
40. **World-Facing Surface Authentication**: Any network-facing HTTP surface (GMCP HTTP, GEMI REST, or any future equivalent) must support a required bearer-token check (`api_auth_token`) and a per-client rate limit (`rate_limit_per_minute`), both config-driven and both checked before any route is dispatched except CORS preflight and an unauthenticated liveness probe. The daemon **seeds** a host token on first start (`SusiConfig::ensure_api_auth_token_seeded` → `~/.susi/api_token`); when a token is configured, every HTTP peer (including localhost) must present `Authorization: Bearer <token>`. Before seeding, non-loopback peers are denied (fail closed on the wire). A world-facing surface with no way to require authentication is not a theoretical gap — this substrate shipped with exactly that gap until it was found and closed.
41. **Untrusted Input Boundary**: Goal/intent text and any LLM-synthesized artifact derived from it (a specialist agent's `description`, a tool argument, a blackboard entry) must be treated as adversarial once it can originate from an unauthenticated or external caller. Concretely: (a) a mutating or destructive operation must never fire on incidental keyword presence in free-text goal input — it requires an explicit, unambiguous intent sentinel (the `admin pulse:` prefix convention); (b) any agent profile synthesized by an LLM from goal text must be sanitized (bounded field lengths, `is_core` forced false, governance-checked) before it is persisted or ever fed back into a future mission's prompt, since persisted state becomes part of every subsequent mission that recruits it. This mandate exists because both failure modes were found present in shipped code and fixed; it generalizes the fix so the same class of bug can't recur in a new code path.
42. **Fallible Error Handling**: Zero `.unwrap()`/`.expect()`/`panic!()` in non-test code, except at a call site carrying a comment stating the specific invariant that makes the panic path unreachable (e.g. "safe: length checked above", "safe: regex is a compile-time literal"). Everywhere else, the fallible path must propagate a real `Result`/`EaiResult` via `?` rather than crash the calling thread — this substrate's daemon and swarm-dispatch paths (Mandate 5, Mandate 13) cannot afford an unrecoverable panic from what should have been a recoverable error. `#[cfg(test)]` code is explicitly exempt: an `.unwrap()` on a known-good test fixture is idiomatic and correct, not a violation — this mandate governs production code paths, not test assertions. This is phrased as a scoped rule rather than a bare "no unwrap, ever" specifically because the latter would be false today (269 call sites existed in `src/` when this mandate was written) and Mandate 1 (No Lies) forbids writing a false claim into this document; this mandate is the target an ongoing triage is converging on, not a claim of present compliance. Four triage batches so far: `EV-2022920-047` (`net_guard.rs` found already fully compliant despite a raw count of 7 — every site inside `#[cfg(test)]`; `reflex_synth.rs` had 3 real non-test sites, 2 fixed and 1 given a justifying comment), `EV-2022920-048` (`qwen2_split.rs` also found already fully compliant despite a raw count of 51; `daemon/server.rs`'s 2 real non-test sites fixed — notably `run_daemon_loop`'s config-load `.expect()`, more severe than a typical unwrap because `panic = "abort"` means it aborted the whole daemon process, not just a thread), `EV-2022920-049` (`agents.rs`'s and `speculative.rs`'s one and two real non-test sites respectively, both verified genuinely safe by construction and comment-justified rather than rewritten), and `EV-2022920-050` (`bloat_audit.rs` found to have zero real sites — its raw count of 42 was almost entirely the tool's own report-label strings describing unwrap/expect/clone counts, not actual calls; `manager.rs`'s 4 real sites are all the same bundled-default-JSON pattern, comment-justified once and referenced from the other three rather than repeated). Roughly 139 sites remain, none in a single concentration larger than ~20-25 — no more single-file "big win" batches remain; further triage means smaller, scattered sweeps.

## 2. Pillar II: THE BODY (Topological Reality)

| Symbol | Tier | Function & Capability |
| :--- | :--- | :--- |
| **GAWD / SMA** | 1 | Swarm Master Authority: `susi-gawd-agents` 0.1 (fleet/peers/detectors) + `susi-gawd-swarm` 0.1 (AMA/AMAS/DAG) + `susi-gawd-a2a` 0.1 (`ra2a` wire) + `susi-gawd` 0.5 host/governance facade. |
| **SusiAdmin** | 1 | Release orchestration, compliance auditing, and axiomatic pulse ingestion. |
| **SusiRuntimeAdmin** | 1 | Substrate maintenance: Hardware audit, model provisioning, and peak selection. |
| **EvolutionManager** | 1 | Substrate self-healing: runs the test-driven evolutionary cycle that is Mandate 20's "Alpha-Self Evolution Order" (not Pillar IV's release "Motion Rule" — see that mandate's naming-collision note). |
| **SusiDaemon** | 1 | Persistent background host bound to `~/.susi`; owns host-contract ports 9090–9093 and spawns from `~/.susi/bin/susi`. |
| **SubstrateKernelLoader** | 1 | Parses the built-in core module manifest list and prints configured port info at startup. |
| **SusiRuntimeAgent** | 1 | Autonomous environment preparation (Weights & Tools). |
| **HardwareAgent** | 1 | Reports the detected hardware profile (CPU, RAM, GPU/acceleration). |
| **SafetyAgent** | 1 | Governance auditor and destructive command interceptor. |
| **SecurityAgent** | 1 | Credential masking and exfiltration prevention. |
| **EvolutionAgent** | 1 | Autonomous drift detection and self-healing agent. |
| **GmcpAgent** | 1 | MCP JSON-RPC 2.0 endpoint verification. |
| **LibraryScoutAgent** | 1 | Live crates.io REST API package scouting. |
| **ContextAgent** | 1 | High-density context manager and workspace analyzer. |
| **NeuralAgentFactory** | 1 | Autonomous synthesis and recruitment of specialist agents. |
| **SUSI-Alpha** | 0 | Microsecond intent classification and deterministic neural reflex engine. |
| **ReflexSynthesizer** | 0 | Native Rust code distillation and reflex generation. |
| **UniversalExecutionSubstrate** | 2 | Hardware-aware inference layer supporting multiple model formats. |
| **GEMI** | 2 | Deep reasoning bridge: crates `susi-gemi-models` (select/provision) + `susi-gemi` engines (run); unified cloud provider inference racing. |
| **SUSI-Vision** | 2 | Candle-based image-feature extractor (untrained linear projection); not yet a trained vision-language model. |
| **SUSI-Audio** | 2 | Candle-based audio-feature extractor (untrained linear projection); not yet a trained acoustic model. |
| **NativeAlphaModel** | 0 | Local neural weights (`susi-alpha.safetensors`) for deterministic reflex. |
| **NativeReasoningModel** | 2 | Distilled Tier 2 logic weights (`susi-reason.safetensors`). |
| **GMCP Server** | 1 | Host-contract Streamable HTTP on **9090** (`/mcp`, alias `/messages`) and alias **9093**; UDP discovery on **9092**. |
| **GMCP Host** | 1 | CLI proxy acting as a protocol bridge. |
| **GEMI Server** | 1 | Host-contract REST on port **9091** (`susi_paths::ports::GEMI`). External clients may hard-code it; the daemon never randomizes or silently drifts this port (same contract as 9090/9092/9093). |
| **EvidenceSession** | 1 | Mission-scoped ledger (`susi-core::capture`): live tool/MCP calls mint `ToolReceipt`s; answers must cite receipt IDs. |
| **TruthTransformer** | 1 | Absolute-truth gate (`susi-core::truth`): accepts only ledger citations, compiled reads, or native verified receipts — models never certify facts. |
| **Evidence IR Substrate** | 1 | Structured provenance types (`EvidenceRecord`, `Claim`, `EvidenceSource::ToolReceipt`) verified against the live ledger. |

## 3. Pillar III: THE MIND (Execution Causal Chain & Priming Spheres)

1. **Pulse Ingestion**: Non-blocking asynchronous listener stages intents in the lock-free `SubstratePulseQueue`.
2. **Swarm Synthesis**: GAWD constructs a Dynamic Execution Graph (DAG), recruiting specialists via `NeuralAgentFactory`.
3. **Truth Convergence**: Swarm outputs pass `TruthTransformer` absolute-evidence gates (live `EvidenceSession` citations, compiled binary reads, or native verified receipts). Soft model review is not a completion path.
4. **The 3 Priming Spheres**:
   - **`global susi` Primes `AlphaSelf`**: The background daemon profiles host hardware/OS, provisions model ladder weights, owns host-contract ports 9090–9093, and maintains global evidence ledgers.
   - **`susi` Primes `AlphaWorld`**: Unconditionally jailed to `cwd`, `susi` indexes local project files, runs unit test audits, and stages intent bundles (`susi accept`).
   - **`susi` Primes `susi repo`**: When `cwd` is the engine's source code, `susi` engages self-evolution, running tests, syncing version manifests (`susi admin sync`), and deploying compiled binaries to `~/.susi/bin/`.

## 4. Pillar IV: THE ENGINE (Substrate Immunity Protocols)

1. **Workspace Purity**: Absolute isolation of artifacts; all ephemeral state contained in `.susi/`.
2. **Hardware Saturation**: Build and execution must maximize hardware utilization without exceeding physical limits.
3. **Motion Rule (Release Gate)**: `susi admin release` (`SusiAdmin::execute_release`) *is* this sequence, not a step within it: `cargo check` (default features + the host's native GPU backend feature, never `--all-features`, since `cuda`/`mkl`/`metal` are mutually exclusive) -> compliance audit -> `cargo test` -> `cargo clippy` (same feature matrix as check) -> ephemeral mission smoke tests -> `susi admin sync` (version manifest sync) -> `git push`. Each step gates the next; a `git push` failure is reported as an error, never silently swallowed. Not to be confused with the unrelated "Alpha-Self Evolution Order" (Pillar I, Mandate 20), which used to share this same "Motion Rule" name.
4. **Conventional Reflex**: Mandatory use of conventional commit prefixes and zero-emoji policy.
5. **Binary Dominance**: Prioritize pre-compiled binary deployment with transparent local build fallback.
6. **Auto-Path Injection**: Installer must inject `.susi/bin` into host environment variables.
7. **Canonical Binary Dynamics & Hot-Reload Protocol**: A single compiled binary deployed at `~/.susi/bin/susi` (the host-contract path). Invocations in `cwd` capture local workspace context, check `~/.susi/substrate.lock`, and ensure `global susi` owns ports/models without duplicate daemons. Detecting binary signature changes (`binary.hash`), `susi` evicts the stale daemon and hot-reloads `global susi` from the canonical binary automatically.
