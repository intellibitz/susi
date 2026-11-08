# Lean core: the program and its ladder

CEO directive, 2026-11-07: Susi core is minimal. GAWD, GEMI, GMCP and the harness
(with its parallel multi-agent workflow) are the only components. It compiles,
builds, tests, runs and debugs anywhere, local or cloud, with nothing but Rust.
Everything that is not one of those four is a plug that lives in
`susi-commercial-apps` and is admitted over the socket. Core is fast, and open
to the world to contribute.

This page is the shared plan for every agent on the program. Numbers come from
`tests/lean_core_ceilings.rs`, which only lets them go down.

## The ladder (measured)

| Rung | Measure (four pillars, normal+build edges) | 2026-11-07 start | now |
|------|--------------------------------------------|------------------|-----|
| 0 | third-party crates compiled | 331 | 259 |
| 0 | workspace crates compiled | 59 | 50 |
| 0 | vendor facades named directly by the pillar crates | 26 | 23 |
| 0 | Rust lines, pillar families (gawd 4, gemi 2, gmcp, harness) | 180,441 | 175,068 |
| clean clone | shallow clone, `cargo check -p susi-gmcp` / `-p susi` | 60 s / 78 s online; offline impossible (crates.io) | same |

Rungs, in order. Each is done when its number is on this table and the ceiling
test pins it.

1. **Product debt out of the pillar closure.** `susi-vendor-{models,agents,web,
   cloud,chrome,candle,fastembed,tantivy}` are third-party product integrations
   (the open-core rule already says they may only shrink). They move to
   `susi-commercial-apps` as plugs; GAWD and GEMI keep the socket contract and
   lose 262, 221, 221, ... crates of reach. **Owner: Antigravity** (crate-level
   migration, gated).
2. **Framework facades replaced by std.** GMCP's hyper/axum/tower/rustls/rmcp,
   GAWD's tokio, the harness's axum become a std-only loopback HTTP/1.1 +
   JSON-RPC socket (precedent: `susi_paths::loopback`, std-only). TLS
   termination leaves core (loopback only; a commercial gateway fronts it).
   This is the engineering rung: weeks, not a night. **Owner: Claude.**
3. **Last dependencies.** serde/serde_json remain or are vendored in-tree;
   the choice is the CEO's: zero crates.io reach with a larger repo, or a
   two-crate floor. Decide at rung 3, not before.
4. **Clean clone, offline.** `cargo build --offline` from a fresh clone with no
   registry. Follows from rung 3.

## Crate classification (93 workspace crates)

Assumption to confirm with the CEO: the `susi`/`susid` binaries stay as the
socket host and composition root (D-SP-4 thin client); "only four components"
means four pillars plus the host that serves them.

| Bucket | Crates | Rule |
|--------|--------|------|
| Keep: pillar | `susi-gawd`, `-gawd-a2a`, `-gawd-agents`, `-gawd-swarm`, `susi-gemi`, `-gemi-models`, `susi-gmcp`, `susi-harness` | shrink; ceilings pin them |
| Keep: foundation under the pillars | `susi-core`, `-error`, `-paths`, `-config`, `-abi`, `-adapters-llm`, `-build-policy`, `-sandbox-client`, `-native-client`, `-http-transport` (until rung 2 replaces it) | shrink |
| Keep: socket host | `susi` (root), `-cli-defs`, `-daemon`, `-server`, `-leaf-services` (collapse the five leaf services into the daemon: one process, no per-service HTTP), `-wire-client`, `-conformance`, `-windows-runtime`, `-build-os-ucrt`, `xtask`, `-unsafe-ratchet` (tooling) | shrink; collapse |
| Keep: Tier 0 | `susi-vendor-wasmtime` (SUSI-Alpha Wasm reflexes are core), `-vendor-syn` (reflex validation) | review at rung 2 |
| Move to commercial (plugs) | `susi-vendor-models`, `-vendor-agents`, `-vendor-web`, `-vendor-cloud`, `-vendor-chrome`, `-vendor-candle`, `-vendor-fastembed`, `-vendor-tantivy`, `-vendor-bollard` + `susi-sandbox` (Docker), `susi-tools` (MCP client adapters; the registry stays in core), `susi-agents` (external-agent plane), `susi-universal-cell` | Antigravity, crate by crate, gated |
| Delete when unreferenced | pure facades nothing reaches after the moves (`anyhow`, `console-subscriber`, `tracing-appender`, `tracing-subscriber`, `md-5`, `rcgen`, `signal-hook`, `tokio-stream`, `criterion`, `proptest` ...) | after each move |

## How work is split

- **Antigravity**: rung 1 and the "move" bucket. Crate-level, one gated ship
  per crate. Before every push: grep every cfg for callers
  (`#[cfg(windows)]` code never compiles on its Linux clone), then
  `cargo check --workspace --all-targets --keep-going` and
  `cargo fmt --all --check` on the exact tree. Push a `local-agent/<name>`
  branch for the laptop gate to integrate; never `main` directly. Lower the
  ceilings in `tests/lean_core_ceilings.rs` in the same change.
- **Claude**: the ceilings, rung 2 (std socket), build speed (the gate's
  concurrent test runner is in; the whole-workspace path and target-level
  narrowing are next), and contributor readiness.
- Both: a change that touches `Cargo.toml` or `Cargo.lock` widens the gate to
  the whole workspace (about 26 minutes); batch manifest changes.

## Contributing from outside

A contributor can clone and `cargo build` online today (measured above). The
gated `cargo local-workflow` path needs a host that holds the gate; an outside
contributor opens a pull request and an agent integrates it through the gate.
Whether to accept outside pull requests at all is a CEO decision; until it is
made, `CONTRIBUTING.md` stands as written.

## Compile budget: 60 seconds (CEO mandate 2026-11-07 22:50)

The measure: a cold `cargo build -p susi` (dev profile, offline, fresh target
directory) of the host binary with the four pillars on the CEO laptop (28
threads). Baseline measured 2026-11-07 22:53 at origin/main `fc3f5a861`,
**under load** (a test gate was running on the same machine; a clean re-run
follows once the gate is idle):

| measure | 22:53 under load, `fc3f5a861` | 23:59 idle, `0710edf64` (iteration 350) | 03:28 idle, landed `b814235c4` | 06:15 idle, landed `66d3ca6d8` | budget |
|---------|----------|----------|----------|----------|--------|
| wall clock | 196 s | 165 s | 104.5 s | 107.7 s (88.9 s in a second idle run) | 60 s |
| rustc CPU-seconds, all units | 2,309 s (478 units) | 1,872 s (471 units) | 1,083 s (462 units) | 990.5 s (427 units) | under 1,200 s |
| of which workspace crates | 393 s | 359 s | 184 s | 219 s | under 150 s |
| of which third-party | 1,916 s | 1,513 s | 899 s | 772 s | under 1,000 s, then towards 0 |
| critical path | 18 units, 196 s | 18 units, 165 s: syn 18 s, `windows` 64 s, then config 10, core 19, gawd-agents 11, gawd-swarm 20, gawd 21, daemon 14, bin 17.5 | 15 units, 124 s: windows-sys 29, tokio 13, h2/hyper/reqwest 18, rmcp 28, gmcp 14, daemon 10, bin 9 | 15 units, 125 s: windows-sys 27, tokio 13, h2/hyper/reqwest 17, rmcp 25, gmcp 15, daemon 11, bin 12 | under 55 s |
| wall seconds with fewer than 4 units compiling (the serial tail) | 77 s | not measured | 29 s | 40 s | under 15 s |

The idle number is the baseline the budget counts from. Between the two
measurements Antigravity's iterations 341-350 removed candle, bollard and the
sandbox crate from the host binary's closure.

Wall is the noisiest column: two idle runs on the same landed commit
`66d3ca6d8` read 88.9 s (04:36) and 107.7 s (06:15), while their unit count is
identical (427) and their CPU-seconds agree within 5% (941 s and 990.5 s).
Sequential builds on a machine other agents also use move wall by up to 20%
with no source change, so the unit count and CPU-seconds are the ratchet and
wall is the number the CEO reads; quote a wall number only with the run it
came from.

Two independent constraints, both binding:

1. **CPU floor.** 2,309 CPU-seconds on 28 threads is 82 s at perfect
   parallelism, so removing dependencies alone cannot reach 60 s until about
   half the CPU-seconds are gone. CPU-seconds is the machine-independent
   number to ratchet; wall is what the CEO sees.
2. **Serial tail.** The last 77 s of the build is one chain:
   `windows` (85 s, pulled by sysinfo) -> susi-config 14 s -> susi-core 21 s
   -> susi-gawd-agents 10 s -> susi-gawd-swarm 17 s -> susi-gawd 17.5 s ->
   susi-daemon 15 s -> `susi` bin 17 s. With every third-party crate gone this
   chain alone is over budget. The pillar crates must shrink (compile time is
   roughly linear in lines: 32k lines is 17.5 s) and the gawd chain must
   flatten.

### Cost by family (CPU-seconds, baseline)

| family | CPU-s | on the critical path | owner of the cut |
|--------|------:|----------------------|------------------|
| workspace crates (pillars, host, facades) | 393 | yes, the whole tail | all: pillar modules leave (table below) |
| HTTP stack: hyper, axum, tower, h2, http, ureq, reqwest | 260 | no | Claude, rung 2 (std loopback socket) |
| crypto: ed25519, x25519, chacha, sha2 (keep, ~60) + rcgen, der, x509 (TLS certs, ~140) | 200 | no | Claude, rung 2 (TLS leaves core: loopback only) |
| proc-macro toolchain: syn, serde_derive, clap_derive ... | 187 | yes, first 33 s | shrinks as derive users leave; syn stays until rung 3 |
| `windows` + windows-sys via sysinfo | 160 | **yes, 85 s** | Claude, lean2 (sysinfo -> std/winapi facade; 7 files) |
| wasm runtime: wasmi + wasmparser | 137 | no | keep (Tier 0 reflexes); audit features |
| tokio runtime | 106 | no | rung 2 |
| json schema: jsonschema, referencing, fancy-regex | 101 | no | Claude, wave 2A (std validator, landed in gmcp; daemon next) |
| serde + serde_json | 91 | no | rung 3 |
| MCP vendor: rmcp, ra2a | 74 | no | Claude, rung 2 |
| docker: bollard (+ its time/icu tail) | 74 | no | Antigravity: susi-sandbox leaves |
| regex | 63 | no | audit users; most are product |
| wasm text format: wast, wat | 0 (was 21.4 s for `wast` at `b814235c4`) | no | done, T-DEEPSEEK-1: the host accepts binary `.wasm` only |
| tracing, clap | 100 | no | keep, trim features |

### What leaves the pillars (module level)

Rule: a module stays in core only if a port cannot serve its protocol without
it. Everything else is a user of the socket and lives in
`susi-commercial-apps`. Proposed split by measured lines (top-level modules,
origin/main `4e8a4eec3`); the CEO decides the exceptions.

| crate | lines | stays (protocol, state machine, socket) | leaves (product) |
|-------|------:|------------------------------------------|------------------|
| susi-gawd | 32,042 | plane_handler, worker_identity, kernel_loader, reflex_verify/reflex_intent (Tier 0 loading), lib | `admin` 15,383, cloud_rsi 1,914, intent_ladder, reflex_trainer, reflex_synth, patch_cycle, eval_*, bloat_audit, repo_gate, rsi_corpus, experiment_*, evolution, genome_distiller, release_qualify, scorecard, dev_canary, telemetry |
| susi-gawd-swarm | 26,710 | dag, parallel_admission, parallel_dispatch, task_lease, quorum_durable, anti_entropy, peer_registry, deliberation, mission_persist/resume, cancel_propagate, worker_recovery, writer_isolation, fair_queue, role_select, resource_schedule, side_effects, capacity_admission, independent_verify, identity_revoke | `ama` 2,921, amas 2,554, roadmap_agents 1,220, cloud_recovery/failover/lockout 3,159, agent_integration 1,096 |
| susi-gawd-agents | 11,480 | agents_plane_handler, scheduler, safety | everything else: `agents` 3,433, `external` 1,279, external_peers, cloud_intent, cloud_budget, system_observe, admin_hooks, pkb, brain, self_core, goal_shape, axiom, live_search, test_plane, agent_meta_registry, adapter_upgrade |
| susi-gawd-a2a | 2,349 | all | mastery_tests |
| susi-gemi | 32,938 | plane_handler, provider_contract, worker, consensus_mode, primary_election, seat_provider, capability_matrix, key_arbitration, context_assembly, usage_accounting, local_tool_calls | `engines` 16,399 (engine implementations behind the contract), credential_scout, scout_*, benchmark, engine_benchmark, operator_report, pulse, model_preload, spend_tracker, orchestration, brain_explain, the *_ext catalogs |
| susi-gemi-models | 17,514 | intent, device, runtime_lifecycle contract types | the rest: `lifecycle` 5,177, hardware, local_ecosystem, hf_discovery, model_license, download*, cloud*, catalogs (Antigravity: crate-level move) |
| susi-gmcp | 18,541 | protocol, jsonrpc_gate, server, stdio, direct_call, content, tool_registry, tool_types, extensions, catalog, mcp_call_budgets, mcp_registry_trust, schema_check | `tools` 10,376 (built-in tool implementations), requirements_ledger, mcp_probe, mcp_profile, mcp_wrapper, eco_mcp_features, embed_provider |
| susi-harness | 40,334 | workflow, adaptive, worker, scheduler, queue, supervisor, verify, ledger, agents, one `susi-harness` binary | adapter 3,330, gateway 2,837, providers 2,633, ingress 2,022, fleet, policy, the operator view module (its UI lives in Susi Commercial Apps), secrets, balance, tools, mcp, operator, roi, smart, budget, channel_bindings, mission_control, cache, ratelimit, the six other binaries |
| host: susi-daemon 25,755, susi-core 32,365, susi-config 17,856 | 75,976 | the socket host: server, supervisor, composition, service_control, security; core bus/registry/commit_log/receipts; config types/config/validate/file_lock | daemon nat, gossip, discovery, estate_loop, provider_ops, os_provision, tls, host_views, resource_governor; core mission_trace, context_graph, mac_policy, peer_admin, truth, audit_export, manifold; config cluster_key, cloud_*, setup_workflow, explain, zc_* |

Target after the split: pillars plus host around 60,000 lines (from 220,000),
which is about 35 CPU-seconds of workspace compile and a serial tail under
15 s. The first 33 s of every build is the proc-macro toolchain; it moves off
the critical path only when serde derive stops gating the first workspace
crate (rung 3).

### Order of cuts (by wall seconds saved per day of work)

1. sysinfo out of the seven files (Claude, lean2): removes the 85 s `windows`
   unit from the critical path. Single biggest lever.
2. Pillar module moves (table above): Antigravity at crate and module level,
   one gated ship per crate, ceilings lowered in the same change.
3. rcgen/TLS, rmcp/ra2a, hyper/axum/tower, ureq, bollard (rung 2 and the
   sandbox move): 600 CPU-seconds.
4. jsonschema in daemon, regex users, clap/tracing features (wast/wat is cut,
   T-DEEPSEEK-1).
5. serde derive off the critical path (rung 3 decision).

### Measuring

`cargo xtask build-time` (alias for `cargo run -p xtask --bin xtask -- build-time`) does the cold build into a temporary target
directory with `--timings`, prints wall, CPU-seconds and the critical path,
and appends a line to `.agents/build-time.jsonl`. The ship gate's own
whole-workspace build already records `timings.jsonl`; the two must agree in
direction. Ceilings for CPU-seconds and critical-path length go into this
table once the clean baseline is recorded.

## Wave log

- 2026-11-07 wave 1 (T-LEAN-1): ceilings test; `susi_core::parallel` (std
  scoped threads) replaces rayon in susi-gawd, susi-gawd-swarm and
  susi-gawd-agents (9 sites); jsonschema (unused) and flume (test-only) edges
  dropped from susi-gemi. Third-party reach 331 to 313.
- 2026-11-07 wave 1, re-pin: Antigravity's iterations 301-310 (ungated) moved the
  `set_selected_model_override` contract into susi-gemi-models (+128 lines) while cutting
  vendor-models; pillar lines now 171,558 (from 180,441), workspace crates 59 -> 58. Rule: a
  pillar may take lines only from a crate leaving core; the change re-pins every ceiling at
  its measured value.
- 2026-11-07: orphan-source ratchet (`tests/orphan_sources.rs`) in core (ceiling 0) and in
  susi-commercial-apps (228 copied files under `crates/*/src/migrated` that nothing compiles).
- 2026-11-07 wave 1, landing: merged onto Antigravity's iterations 311-320 (ungated;
  `susi-vendor-models` deleted from core, its contracts moved into susi-gemi-models, +7,986
  lines, allowed by the rule above). Measured on the merged tree: third-party 307, workspace
  crates 54, direct facade edges 25, pillar lines 179,544; every ceiling re-pinned there.
- 2026-11-07 wave 1, second landing: iterations 321-330 (ungated; vendor-agents, susi-agents and
  vendor-web deleted from core, contracts re-homed in gawd-agents) merged in; workspace crates
  54 -> 52, pillar lines 181,798; re-pinned. Lesson: an exact line pin loses every race against
  an ungated push into a pillar; the ratchet still holds, the landing just re-pins.
- 2026-11-07 wave 1, third landing: iterations 331-340 (ungated; vendor-cloud, vendor-chrome,
  fastembed, tantivy, universal-cell deleted from core; susi-gmcp +118 lines of decoupling)
  merged in and re-pinned. Also fixed in this wave: gawd-agents tests rewrote `SUSI_HOME` under
  two different locks and raced in the gate; one shared lock now.
- 2026-11-07 wave 1, fourth landing: iterations 341-350 (ungated; candle, bollard and the
  sandbox crate out of core, GEMI local-engine modules re-homed in commercial) merged in:
  susi-gemi 32,938 -> 26,548 lines, susi-gemi-models 17,514 -> 17,056, third-party 307 -> 304,
  pillar lines 175,068; ceilings lowered to the measured values.
- 2026-11-08 wave 1 + Gemini wave, batch landing: Gemini's std replacement of parking_lot,
  dashmap and flume costs susi-gawd-swarm 8 lines (lock guards where DashMap entries were);
  pinned at the combined tree's values. The exact line pin has now lost to three ungated pushes
  and one legitimate cut; the next change makes it a per-change delta against the merge-base.
- 2026-11-08 wave 1, batch landing: iterations 351 and 352 (MCP client adapter stubbed, the
  rmcp facade deleted, "migrated to commercial" with nothing on the commercial origin) are
  reverted in core until a plug or the destination exists: `tests/mcp_call_cli_tests` failed
  on plain main. The third-party count goes back up by the rmcp family for now; the mandate
  (workspace AGENTS.md) forbids this kind of deletion from here on.
- 2026-11-08 02:00, CEO decision "land with ignores": `tests/zc_matrix.rs` marks the two
  Rust-model-runtime tests ignored (reason names iterations 341-347 and the restore list) so the
  two waves land; the runtime's restoration is a separate gated change.
- 2026-11-08 wave 2 (T-LEAN2-1): the sysinfo facade is gone (host metrics and the process probe
  read /proc and Win32 through the winapi facade's new `host` module), the 85-second `windows`
  unit leaves the critical path; GMCP's std JSON Schema validator replaces jsonschema; the
  orphan-source ratchet; `cargo xtask build-time`; the harness `IntegrationPolicy` (remote-agent
  mandate). Third-party 295, workspace 51, direct facade edges 24.
- 2026-11-08 wave 3 (T-LEAN3-1): landed main measured at 104.5 s wall / 1,083 CPU-seconds (from
  165 / 1,872 at iteration 350): the sysinfo cut and Antigravity's crate deletions. The unused
  jsonschema line leaves GMCP's manifest; `cargo xtask` alias added. Third-party 268.
- 2026-11-08 binary `.wasm` only (T-DEEPSEEK-1, landed `66d3ca6d8`): the socket no longer compiles
  the WebAssembly text format. `susi-vendor-wasmtime` refuses any input without the binary magic
  at both entry points (`resources::check_binary_module`, from `cell::instantiate` and
  `wasm::execute_reflex`), and `wat` moved to a dev-dependency of that crate, so `wat` and its
  `wast` parser left the host closure: 5 units (462 -> 457 on its own base `b814235c4`; the landed
  tree measures 427 units and 990.5 CPU-seconds, after wave 3's jsonschema cut landed too). The
  daemon's crown sandbox probes carry the compiled bytes of their fixtures with the WAT source in
  doc comments, and a new test (`sandbox_check_runs_the_embedded_binary_probes`) runs them through
  the live sandbox check. No ceiling moved: `susi-vendor-wasmtime` is not in the four pillars'
  closure, so the third-party count, the workspace count and the line pins stay where wave 3 left
  them. Two build-time runs at `6f1df35c7` (126.2 s / 1,397.6 CPU-s and 134.9 s / 1,302.8 CPU-s)
  were taken while other agents' gates held the CPU; they are not budget numbers.
- 2026-11-08 rung 1 MCP client cut (T-ANTIGRAVITY-1, commercial commit `dc623f2`): `susi-tools` serves `McpServerConfig`, `call_blocking_result` and `list_tools_blocking` over `PlaneBus` (`ecosystem_hub.mcp.call` / `ecosystem_hub.mcp.list_tools`) and a std JSON-RPC 2.0 stdio/HTTP client; `susi-vendor-mcp` and the unreached `susi-vendor-jsonschema` facade leave core. Third-party 268 -> 259, workspace 50, direct facade edges 23.
- 2026-11-08 iteration 362 (Antigravity, adopted through the gate): the MCP SDK facade
  (`susi-vendor-mcp`, rmcp) replaced by a plane-bus plus std JSON-RPC client in susi-tools; the
  jsonschema facade crate deleted. Third-party 259, workspace 50, facade edges 23.
