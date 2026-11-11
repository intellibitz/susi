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

| measure | 22:53 under load, `fc3f5a861` | 23:59 idle, `0710edf64` (iteration 350) | 03:28 idle, landed `b814235c4` | 06:15 idle, landed `66d3ca6d8` | 09:01 idle, landed `240ee4dfb` (iteration 362) | 21:21-21:30 idle, T-CORENET-1 `437ccf597` (three runs alternating with main `92f7ee498`) | 06:18-06:26, no other build or gate at each run's start, T-A2ASTD-1 `3acbb47b3` (two runs alternating with main `aae59f4c2`) | 07:12-07:18 idle (nothing else building at each start), T-NOMCPSDK-1 `47e7a8137` (two runs alternating with main `5f23245e0`) | budget |
| --------- | ---------- | ---------- | ---------- | ---------- | ---------- | ---------- | ---------- | ---------- | -------- |
| wall clock | 196 s | 165 s | 104.5 s | 107.7 s (88.9 s in a second idle run) | 81.7 s | 70.1, 70.2, 70.6 s (main: 74.2, 73.3, 76.9 s) | 83.1, 86.2 s (main: 83.8, 82.8 s; 78.3 s at `c70d17be1` 05:56) | 74.7, 74.6 s (main: 80.4, 84.7 s) | 60 s |
| rustc CPU-seconds, all units | 2,309 s (478 units) | 1,872 s (471 units) | 1,083 s (462 units) | 990.5 s (427 units) | 822.7 s (417 units) | 775.8-784.7 s (417 units; main 745.8-774.9 s) | 893.3, 896.8 s (417 units; main 955.3, 909.1 s, 417 units) | 813.1, 788.0 s (389 units; main 845.5, 895.9 s, 417 units) | under 1,200 s |
| of which workspace crates | 393 s | 359 s | 184 s | 219 s | 148.4 s | 157.9-161.5 s (main 153.4-162.3 s) | 186.5, 190.4 s (main 203.5, 191.9 s) | 181.6, 181.5 s (main 174.3, 190.3 s) | under 150 s |
| of which third-party | 1,916 s | 1,513 s | 899 s | 772 s | 674.2 s | 617.7-624.0 s (main 592.4-612.7 s) | 706.8, 706.3 s (main 751.7, 717.2 s) | 631.5, 606.5 s (main 671.2, 705.6 s) | under 1,000 s, then towards 0 |
| critical path | 18 units, 196 s | 18 units, 165 s: syn 18 s, `windows` 64 s, then config 10, core 19, gawd-agents 11, gawd-swarm 20, gawd 21, daemon 14, bin 17.5 | 15 units, 124 s: windows-sys 29, tokio 13, h2/hyper/reqwest 18, rmcp 28, gmcp 14, daemon 10, bin 9 | 15 units, 125 s: windows-sys 27, tokio 13, h2/hyper/reqwest 17, rmcp 25, gmcp 15, daemon 11, bin 12 | 17 units, 95.1 s: windows-sys 19 s -> tokio 15.5 -> h2/hyper/hyper-util 8.8 -> susi-core 10.6 -> gawd-agents 4.4 -> gawd-swarm 7.6 -> gawd 8.1 -> daemon 6.0 -> bin 7.5 | 15 units, 81.5-82.1 s: windows-sys 19 -> tokio 9.5 -> h2/hyper/hyper-util 9.7 -> axum 5.7 -> ra2a 7.7 -> gawd-a2a 2.7 -> gawd 8.9 -> daemon 6.0 -> bin 7.4; susi-core starts at 29.7-30.2 s (main 36.8-38.7 s) and is off the path (main: 17 units, 88.5-92.7 s, through susi-http-transport -> susi-adapters-llm -> susi-core) | 10 units, 93.6-94.6 s: windows-sys 22 -> tokio 11-13 -> tokio-util 1 -> rmcp 21-23 -> gmcp 16 -> daemon 8-9 -> bin 8-9, the same path as main (main 95.1 s; one main run read 103.9 s through the serde/config/core/gawd chain); susi-gawd no longer waits on susi-gawd-a2a | 14-16 units, 91.3, 88.1 s: syn 12 -> serde_derive 7 -> config 9.5 -> core 14.5 -> gmcp 19.7 -> daemon 8.7 -> bin 7 (rmcp gone; main 10 units, 86.9, 95.3 s: windows-sys 21 -> mio/tokio/tokio-util 15 -> rmcp 19.6 -> gmcp 15.8 -> daemon 7.8 -> bin 7.2) | under 55 s |
| wall seconds with fewer than 4 units compiling (the serial tail) | 77 s | not measured | 29 s | 40 s | 29.9 s | 17.8-18.1 s (main 24.1-25.1 s) | 18.0, 20.3 s (main 18.0, 16.9 s) | 16.8, 17.5 s (main 20.0, 16.7 s) | under 15 s |

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
| MCP vendor: rmcp, ra2a | 74 (0 since T-CLAUDENOA2ASDK-1: GMCP serves MCP and the A2A tier serves A2A itself) | rmcp was, 2026-11-09 | done (Claude, rung 2) |
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
| susi-gawd-swarm | 26,710 | dag, parallel_admission, parallel_dispatch, task_lease, quorum_durable, anti_entropy, peer_registry, deliberation, mission_persist/resume, cancel_propagate, worker_recovery, writer_isolation, fair_queue, role_select, resource_schedule, side_effects, capacity_admission, independent_verify, identity_revoke | left (T-SWARMMOVE-1, to Susi Frontier Lab): roadmap_agents 1,220, agent_integration 1,096. Kept after review: amas 2,554 (cluster protocol: discovery, attested peer dispatch, leader election, lock broadcast), cloud_failover/lockout (parallel_dispatch is built on them), `ama` 2,921 with cloud_recovery (the mission engine behind `gawd.solve`; moving it is an open CEO decision) |
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

### Linux x86_64 (T-BUILDTIME-1, 2026-11-11, under load)

First measurements on Linux: the CEO laptop under CachyOS (i7-13850HX, 28
threads, 219 GiB, rustc 1.99.0), with other agents compiling throughout. Load
average (1 min) ran 7 to 53 and 0 to 36 other rustc processes; every number
here is **under load**, none is an idle baseline. Base origin/main
`d771008a3`; "16 units" is `[profile.dev] codegen-units = 16` (`eca904a44`,
or the same setting by `CARGO_PROFILE_DEV_CODEGEN_UNITS` on the base).

| measure | result |
| ------- | ------ |
| cold `cargo build -p susi` (`cargo xtask build-time`), base, 256 units | 82.3, 99.6, 81.0, 148.6, 110.8, 80.5, 78.9, 128.5 s wall; 736-1,256 CPU-s; 395 units |
| same, 16 units | 98.1, 110.7, 95.3, 76.0, 146.8, 102.5, 75.6, 178.1 s wall; 690-1,190 CPU-s; 395 units |
| alternated pairs (256 then 16, median load 1 of each) | 148.6 (43) / 110.7 (44); 110.8 (38) / 95.3 (35); 80.5 (30) / 76.0 (30); 78.9 (31) / 75.6 (28); 128.5 (32) / 178.1 (48) |
| warm no-op `cargo build -p susi` | 0.22, 0.23 s |
| warm, `touch` a susi-gmcp source | 4.4-8.0 s (256); 4.3, 4.9 s (16) |
| warm, `touch src/main.rs` | 2.8-8.1 s (256); 2.1, 2.5 s (16) |
| warm, `touch` susi-core `lib.rs` | 6.5-8.5 s (256); 6.2, 10.4 s (16) |
| `cargo test -p susi-gmcp --no-run` right after `cargo build -p susi` | **78.2 s, 162 crates recompiled (256)**; **32.6 s, 49 crates (16)** |
| `cargo test -p susi-gmcp`, warm (222 tests, 4 binaries) | 17.1 s (doc-tests compiling), 5.7 s |
| change to result: one-line edit in susi-gmcp, `cargo build -p susi`, `cargo test -p susi-gmcp` (both already warm) | 22.6, 29.9, 19.8 s (256); 23.3, 25.4, 19.9 s (16) |
| `target/debug/susi --version` | 40-61 ms |
| daemon `susi start` to all ports listening (isolated home, offset 3000) | 0.75 s fresh home, 0.39 s second start; `susi stop` 0.30 s |
| `susi start` -> MCP `initialize` -> `tools/list` answered | 0.52 s |

Reading: the cold build is 75.6-178.1 s on this laptop under load, against the
60 s budget; the best runs (75.6-82.3 s) sit with the Windows numbers. The 16-unit
setting won four of five alternated pairs; the one loss ran at a median load of
48 against 32. In the two best-matched pairs (load about 30 in both) it was 4-6%
faster in wall and 6-11% lower in CPU-seconds: within the noise of this
machine, so it counts as "no regression", not as a cut. The deterministic gain
is the test step: before, `[profile.test]` had 16 units (`487c36292`) and
`[profile.dev]` 256, so every dependency had two fingerprints and the first
`cargo test` after a `cargo build` rebuilt the graph. A separate Linux record
by another method (`T-GROKSTEP1LINUX-2`, `.agents/evidence/step1-linux-T-GROKSTEP1LINUX-2.json`:
base `de22ea776`, `-j 8`, load 5-22) reads 71.8 s cold.

Critical path on Linux (timings of a 256-unit run, 81.0 s): syn (host) 2-13 s
-> serde_derive 13-22 -> serde, susi-error -> susi-config 24-35 (frontend 9.1 s)
-> susi-core 33-50 (frontend 13.5 s) -> three branches that meet at about 67 s:
susi-gmcp 47-70 (frontend 19.7 s), gawd-agents -> gawd-swarm -> susi-gawd
(47-69), susi-gemi 55-67 -> susi-daemon 67.5-75 -> `susi` bin 75-81. Cargo
pipelines on metadata, so the tool's summed "critical path" (88-202 s) is an
upper bound, not the wall. Levers, by wall saved: (1) the pillar module moves
(core, gmcp and config frontends are 42 s of the path; CEO-decided moves in
"What leaves the pillars"); (2) the proc-macro gate (22 s before the first
serde-using workspace crate starts; rung 3); (3) the tail after the branches
meet (daemon 7 s plus the `susi` binary 6 s). Off the path but CPU: syn is
compiled three times (2.0.119 and 3.0.9 for proc-macros, 3.0.9 again for
`susi-vendor-syn` in susi-gawd), openssl via native-tls (Linux only, through
reqwest in ra2a), wasmparser and wasmi.

Low load (T-CORECUT-1, 2026-11-11 03:2x-03:42, same laptop, empty target
directories, `--offline --locked`): **28.3 s** cold at load average 3.2 with no
other rustc running, 41.1 s at 6.9, 42.0-44.5 s at 9-12, against 75.6-178.1 s
above at 7-53 with other agents compiling. On an idle Linux laptop the 60 s
budget holds; the misses are machine load, so CPU-seconds matter as much as
the path. At load 7 the serial path read: macro front (syn -> serde_derive ->
serde -> susi-error) 8.2 s -> susi-config to metadata 3.6 s -> susi-core 6.7 s
-> three branches tied within 1 s (gawd-agents -> gawd-swarm -> susi-gawd 12.8 s;
susi-gmcp 11.8 s; susi-gemi-models -> susi-gemi 11.8 s; susi-harness 9.5 s) ->
susi-daemon 6.0 s -> `susi` binary 3.8 s (1.1 s of it the link). Because the
middle branches are tied, shortening one alone saves nothing (why the swarm
and A2A moves measured no wall gain); the serial pieces are the front,
config, core and the tail.

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
- 2026-11-08 09:01 landed `240ee4dfb` (iteration 362 adopted) measured idle, nothing else on the
  machine: 81.7 s wall, 822.7 rustc CPU-seconds, 417 units (from 88.9 s / 941 s / 427 at `66d3ca6d8`),
  serial tail 29.9 s. The MCP client facade is gone from the path, but `rmcp` is still compiled
  (13.8 CPU-s) through `susi-vendor-mcp-server`, the GMCP server side's SDK facade. The critical
  path is now windows-sys 19 s -> tokio 15.5 -> h2/hyper/hyper-util 8.8 -> the pillar chain
  (core 10.6, gawd-agents 4.4, gawd-swarm 7.6, gawd 8.1, daemon 6.0, bin 7.5). Heaviest by CPU:
  syn 34, wasmparser 26, windows-sys 19, serde_core 16, tokio 15.5, rmcp 14, sha2 14, zerocopy 13.
  Next levers in order: the MCP server facade (rung 2 std socket or destination-first), the
  tokio/hyper stack (rung 2), windows-sys (the daemon's Win32 surface through `susi-vendor-winapi`
  only), and syn (rung 3, derive-free serde).
- 2026-11-08 11:50 (T-PLUGPORTS-1): the two remaining "moved to commercial" errors became
  plane-bus plug ports (embeddings `gpu_fabric.embed.*`, browser `desktop.browser.capture`,
  served by the commercial `susi-plug-host` over core's own bus rendezvous), a default build
  forwards local inference to a wired plug, and a global config read never seeds `config.json`
  (the cause of the 35-minute gate stall: a sibling test's read emptied a just-saved config).
  The pillar line ceiling now counts shipped code only: the `tests/` and `mastery_tests/`
  directories directly under `src` hold standalone `#[cfg(test)] #[path]` suites and are left
  out (35 such suites are returning from the commercial copies next), inline `mod tests` still
  count, and every pin is re-based at the measured shipped value (gawd 29,296, gawd-swarm
  22,943, gawd-agents 11,495, gawd-a2a 2,108, gemi 21,304, gemi-models 16,797, gmcp 19,344,
  harness 40,790); gmcp and gemi-models carry the plug ports and the bounded ladder test.
- 2026-11-08 13:05 (T-CORETESTS-1): 35 test suites whose only subject is core came back from the
  commercial copies (23 GAWD swarm suites, the member-identity ledger, the gated dev workflow,
  the installers, the A2A executor and the extensions state lock; the CEO chose the return over
  a copy of the swarm in the paid suite). They live in `src/tests/` and `src/mastery_tests/`
  as before and are excluded from the shipped-line measure; only their `#[cfg(test)] #[path]`
  declarations count: susi-gawd-swarm 22,943 -> 23,012, susi-gawd-a2a 2,108 -> 2,109.
- 2026-11-08 (T-PEERSEC-1): peer credential hardening shrinks two pillars. The scout's discovery
  send collapses into one bound-ping loop (`SUSI_PING_SIG3`, one frame per route source) and its
  unsigned `SUSI_PING` broadcast, which every daemon already dropped, is gone; the bootloader's
  probe documentation is tighter. The ping protocol itself lives in `susi-config`, outside the
  pillars. Re-pinned at the measured values: susi-gawd 29,296 -> 29,295, susi-gawd-swarm
  23,012 -> 23,000.
- 2026-11-08 (T-HOSTONLY-1): the `host_only` privacy mode (CEO decision 19:4x) is a new security
  capability, so three pillars take the few lines it needs at their own boundaries; the policy
  itself (loopback-only reach, grant refusal, the mode reads) lives in `susi-core` and the
  daemon, outside the pillars. The scout reads the mode once per pass and sends and reads no
  discovery datagram under it (susi-gawd-swarm 23,000 -> 23,002); the A2A and GMCP listeners
  drop an off-host client before negotiating (susi-gawd-a2a 2,109 -> 2,113; susi-gmcp
  19,344 -> 19,350, of which 4 lines are the `privacy_mode` test of the new spelling); the GEMI
  provider gate moves onto the mode-aware check and shrinks (susi-gemi 21,304 -> 21,302).
  Re-pinned at the measured values.
- 2026-11-08 21:30 (T-CORENET-1): susi-core leaves the async runtime and the HTTP stack. The
  peer MCP client runs on `susi_core::plain_http` (std `TcpStream`, plaintext `http://` only,
  `https://` refused by name, one budget over connect, send and body, Content-Length, chunked
  and until-close bodies, redirects never followed as before, no proxy); provider futures are
  runtime-agnostic (`provider::run_blocking` threads and `provider::block_on`), so the owner-side
  provider dispatch builds no runtime; the never-set async evidence task-local and the truth
  verifier's runtime are gone (`cross_examine` is synchronous); `inference_wire` is imported from
  `susi-adapters-llm`, its real home. susi-core's normal dependencies lose susi-http-transport,
  susi-adapters-llm, susi-vendor-tokio and three unused facades (dashmap, flume, parking_lot);
  `cargo tree -p susi-core -e normal` names no tokio, hyper, h2, mio, ureq, reqwest, rustls or
  native-tls. Measured idle, alternating with main `92f7ee498` on the same worktree and F: drive:
  wall 70.1 / 70.2 / 70.6 s against 74.2 / 73.3 / 76.9 s; susi-core starts at 29.7-30.2 s against
  36.8-38.7 s; serial tail 18 s against 24-25 s; the critical path no longer runs through
  susi-core (now windows-sys -> tokio -> h2/hyper -> axum -> ra2a -> gawd-a2a -> gawd -> daemon ->
  bin, 81.5-82.1 s). CPU-seconds rise about 3% (775.8-784.7 s against 745.8-774.9 s, same 417
  units): more units now overlap on the 28 threads. Runs at 20:46-20:52 (two of them read
  83.6 / 85.8 s) were not checked for other builds and a commercial release build ran from 20:52,
  so they are not counted. At this landing two gates remained between susi-core and an earlier
  start: the `cell-server` feature of susi-abi, which unified tokio into susi-abi; and
  susi-gawd-agents' own susi-http-transport edge. (Corrected 2026-11-09, T-A2ASTD-1: the first
  no longer exists. Since `d14dfe5ee`, T-COMPILE2-1, susi-abi's cell server is std-only and the
  feature with its tokio edge is gone, so tokio reaches neither susi-abi nor susi-core; core
  now waits on susi-config, not tokio.) The
  pillar reach does not move (50 workspace, 259 third-party, 23 facade edges: core is not a
  pillar and the pillars still use the stack). The susi-gemi line ceiling goes 21,302 -> 21,300
  (runtime-free candle provider futures; the scout probe test now holds the env lock, whose
  absence let it overwrite the brain backup test's file mid-recovery).
- 2026-11-09 03:4x (T-DEVWF-1): the developer workflow leaves core. `susi-gawd/src/admin`
  (task queue, worktree workflow, hook guard, primary parking, target seeding, version sync,
  release gate) with `worker_identity`, `cloud_session` and `zc_agent_identity`, about 16,300
  lines, moved destination-first to susi-commercial-apps (merge `5c4793332`, change `ae20411`,
  gate receipt `3b09dbb72`): crate `susi-dev-workflow`, commands `susi-commercial tasks |
  workflow | admin sync | verify | release | intent-ladder --adopt-gap`, with the module's
  suites and the ported tasks CLI suite. Core keeps `susi_gawd::compliance` (the compliance
  audit, whose version step is now read-only, and the natural-intent evidence ledger) and
  `susi tasks eval-corpus`. Every dropped command refuses naming its commercial command
  (`susi tasks <verb>`, `susi workflow <verb>`, `susi admin sync | verify | release`,
  `--adopt-gap`, the host view's `adopt_gap` mode); every verb of the moved families stays
  refused as a bare word, which would otherwise become a mission. Hooks and scripts probe
  `susi-commercial` first; with no binary that has `workflow guard`, the pre-commit /
  pre-merge / pre-push guard says so and lets git go on (`cargo local-workflow` never runs
  those hooks and gates every landing), so a contributor without Susi Commercial Apps commits
  unguarded. `local-workflow start` seeds a new worktree through whichever binary answers
  `workflow seed-target`. Tests of the moved product left with it; core keeps its own e2e
  checks of the built binary (Mandate 53, now with the refusals) and pins the task-record
  schema without the moved crate. susi-gawd drops its only `susi-vendor-winapi` edge (the
  seed's detached spawn). Ceilings: susi-gawd lines 29,291 -> 13,293, direct facade edges
  23 -> 22 (workspace 50 and third-party 259 unchanged: the pillars still reach winapi).
  Measured alternating with main `c60bb469f` on F: before compile2's landing (rustc idle
  before each run, but 850-1,237 CPU-s against about 780-800 on an idle host, so these walls
  are not budget numbers): wall 90.9 / 78.6 / 90.6 s against 124.0 / 83.2 / 91.4 s; workspace
  CPU-s 208.1 / 177.0 / 179.5 against 279.9 / 190.9 / 207.8 (lower in every pair); same 417
  units. Not done here: the GMCP `intent_ladder` tool description and the queue example in
  `docs/gmcp-roadmap.md` still name the moved commands (both are pinned by the GMCP ledger and
  evidence and change only with both re-record phases), and Mandate 55's did-you-mean example
  in `.agents/identity.json` still names `susi admin release`. (Done 2026-11-11, T-TRUTHDOCS-1:
  the `intent_ladder` description says `adopt_gap` is refused and names the commercial
  command, the roadmap queue example names `susi-commercial`, and Mandate 55 names
  `susi-commercial admin release`; GMCP evidence re-recorded in both phases.)
- 2026-11-09 T-PEERCHAN-1 (sealed peer channel, `docs/peer-channel.md`): `susi-gmcp` re-pinned
  19,345 -> 19,469 (+124), at the measured value, as the brief for this task allows. The lines are
  the GMCP side of the channel in `server.rs` (sessions that terminate at this node are served
  over an in-process socket pair with the identity their handshake proved, authorized by
  `NetGuard::channel_authorized`; their `Host` is the loopback name) and the `member_propose`
  fix in `tools/core.rs` (the leader is counted once, which made every proposal to a standalone
  leader fail; a committed admission is pushed to every bound member). The channel itself (Noise
  `XXpsk0` over the node keys, links, relay, uplinks) lives in `susi-config`, `susi-core` and the
  daemon, outside the pillar ceilings. No crate is added: pillar reach stays 50 workspace, 259
  third-party, 22 facade edges. GMCP evidence re-recorded in both phases with the change.
- 2026-11-09 T-A2ASTD-1 (the A2A HTTP stack off the path into susi-gawd): susi-gawd used no item
  of susi-gawd-a2a and only re-exported it (`susi_gawd::a2a`, `capabilities`, `executor`,
  `server`, `task_store`); the daemon was the one caller. The dependency moves from susi-gawd to
  susi-daemon, which now serves the A2A port (9094 + offset) through `susi_gawd_a2a::server`
  directly. No crate is added, no third-party crate is added, and no line of the A2A server
  changes, so bearer auth, `host_only`, `bind_address` and the protocol are untouched; the live
  host-contract conformance run (`susi-conformance --port-offset 700`, isolated home) gives the
  same 24 checks and outcomes on main `aae59f4c2` and on the branch, A2A card, unknown method,
  `/stream` codes and the 401s on `/` and `/stream` included. A std rewrite of the A2A server
  was not taken: the daemon keeps tokio, hyper and axum through susi-gmcp, susi-harness and
  susi-http-transport anyway (`cargo tree -p susi -i hyper`), so it would only remove ra2a and its
  client tail while re-implementing the A2A task store, SSE streaming and push configuration.
  Pillar reach: 259 -> 231 third-party (ra2a with its reqwest/rustls/hyper-tls client side,
  tower-http, the windows 0.52 targets), 50 -> 47 workspace (susi-gawd-a2a, susi-vendor-ra2a,
  susi-vendor-tower), 22 facade edges unchanged; the host binary still compiles the same 417
  units, because the daemon reaches them now. susi-gawd lines 13,293 -> 13,288. Measured
  06:18-06:26 (no other build or gate at each run's start), alternating branch `3acbb47b3` and
  main `aae59f4c2`: wall 83.1 / 86.2 s against 83.8 / 82.8 s, CPU-s 893.3 / 896.8 against
  955.3 / 909.1, all 417 units: no wall gain, as expected. On this tree susi-gawd was already
  unblocked by susi-gawd-swarm, with susi-gawd-a2a ending 0.1-1.7 s before it; after the change
  susi-gawd-a2a feeds only the daemon and ends 5.5-6.5 s before the daemon starts, so a shorter
  gawd chain (swarmmove, config and core shrink) is no longer capped by the A2A chain. The
  critical path in every run since peerchan is windows-sys -> tokio -> rmcp -> susi-gmcp ->
  susi-daemon -> bin (93.6-95.1 s of units), which is what stands between the build and 60 s
  next, together with the serial gawd chain behind it.
- 2026-11-09 T-SWARMMOVE-1 (swarm product modules, destination first): the parallel roadmap
  agents (`roadmap_agents`: task queue, claims, private worktrees, fenced leases, `run_round`)
  and their integration gate (`agent_integration`: acceptance, independent verification,
  verified closure, merge without silent overwrite) leave susi-gawd-swarm with their suites
  `parallel_roadmap_e2e` and `swarm_gap_verified_closure`; the implementation section of
  susi-gawd's `cloud_rsi` (`CloudImplExec`, `ImplModel`, `Delegator`,
  `delegate_capability_gaps`) leaves with them. No binary called any of it. The destination
  landed first: commercial `029b5e0` (gated merge `b9a23c4`, receipt `a2d40ae`; 67 test
  binaries, 1,718 passed, 0 failed) declares them in susi-frontier-lab, where the 54 tests of
  the moved files pass unchanged; the executor applies plans through core's own GMCP
  `apply_patch_cycle` tool over the socket, checked live against an isolated core (offset 700),
  so the patch cycle stays here. The rest of the swarm row stays for now, on evidence: `amas`
  is cluster protocol (peer_registry, deliberation and the daemon build on it), `cloud_failover`
  and `cloud_lockout` carry parallel_dispatch's `Runner`, `FailoverBudget` and `LockoutTracker`,
  and `ama` with `cloud_recovery` is the mission engine behind `gawd.solve`, the daemon and the
  CLI, which stays in core until the CEO decides. Roadmap vectors VC-201-027 and VC-201-028
  record where their evidence now runs. Lines: susi-gawd-swarm 23,002 -> 20,673, susi-gawd
  13,288 -> 12,653; pillar reach unchanged (47 workspace, 231 third-party, 22 facade edges).
  Measured idle on F: after the landing (07:56-08:30; each run started only after 120 s with no
  rustc, cargo or link process on the machine and was kept only if no other agent's rustc ran
  during it; four contaminated runs were discarded), main before `f0a182b92` against after
  `51ca610b9`, alternated with "before" first three times, then "after" first twice: before
  70.9 / 74.1 / 76.1 / 78.2 / 77.1 s, after 78.5 / 75.4 / 78.6 / 79.1 / 91.8 s, all 417 units.
  No gain is measurable: the extra CPU in the "after" runs falls on third-party crates that are
  byte-identical in both trees (tokio, wasmparser, wasmi), and the susi-gawd-swarm unit reads
  8.9-10.2 CPU-s before against 9.4-11.5 after, so run-to-run noise of about 8 s hides a cut
  whose expected size is under a second (2,324 lines at about 0.37 ms per line). The idle check
  watched compilers only, not test binaries other agents ran, which is the likeliest source of
  that noise. The 60 s budget needs the larger moves still open: the mission engine (`ama`,
  CEO decision) and the rmcp/tokio chain that now leads the critical path.
- 2026-11-09 T-NOMCPSDK-1 (the MCP server SDK leaves core): GMCP serves MCP with its own
  implementation in `crates/susi-gmcp/src/mcp` (wire model, JSON-RPC envelopes with rmcp's parse
  strictness, the `initialize` and 2026-07-28 per-request lifecycles, tools/list paging,
  tools/call with progress, cancellation, tasks and multi-round-trip input, resources, prompts,
  completion, logging, subscriptions; the stdio gate and Streamable HTTP with sessions, SSE
  priming events and keep-alive, stateless 2026-07-28 requests, `Mcp-*` header checks, Host and
  Origin validation) on the tokio/hyper transport GMCP already ran. The 200 built-in tool
  definitions come from `tool_def!` and their 119 argument shapes from `input_schema!`, which
  reproduce every previous `inputSchema` exactly. `susi-vendor-mcp-server` is deleted; rmcp,
  rmcp-macros, schemars, schemars_derive, sse-stream, pastey, darling and 11 more lock entries
  leave the build. No crate or third-party dependency is added. Behaviour, recorded before and
  after on isolated homes: 23 stdio scenarios (both eras, malformed input, pre-initialize
  traffic, tasks, subscriptions) and 48 HTTP exchanges (status, headers, SSE event ids and
  bodies) are identical byte for byte, the 195/200 tool definitions (default/`tools-rich`) and
  `server/discover` are identical, the host-contract conformance run gives the same 24 outcomes,
  and the commercial live contract test passes 24/25 against a daemon built from the branch
  (the one failure is the certified-release pin, which names a published binary). Pillar reach:
  231 -> 198 third-party, 47 -> 46 workspace, 22 -> 21 facade edges. Lines: susi-gmcp
  19,469 -> 25,900 (+6,431, the protocol that rmcp's 55,000 source lines used to carry); this is
  a re-pin upward under the "crate leaving core, contract moving in" rule and is the trade the
  CEO should see. Measured 07:12-07:18, nothing else building at each run's start, alternating
  branch `47e7a8137` and main `5f23245e0`: wall 74.7 / 74.6 s against 80.4 / 84.7 s, CPU-s
  813.1 / 788.0 against 845.5 / 895.9, 389 units against 417. The critical path no longer
  passes rmcp; it now runs syn -> serde_derive -> susi-config -> susi-core -> susi-gmcp (19.7 s,
  from 15.8 s) -> susi-daemon -> bin. Next levers for 60 s: the proc-macro/serde chain gating
  susi-config, susi-core and susi-gmcp's own compile time, windows-sys, and tokio/hyper (rung 2).
- 2026-11-10 T-DOTHARNESSNATIVE-1 (native harness without growing pillar reach):
  the standalone `susi-harness` remains in the harness pillar and keeps its package,
  executable name and build command. It now uses the existing original Rust Windows
  entry/TLS and OS-UCRT build boundary, instead of requiring `vcruntime140.dll`.
  Those two real workspace dependencies are counted, not hidden behind an optional
  feature or moved outside the measured product. Two obsolete compile edges leave:
  `susi-gawd-swarm` never references its declared flume dependency, and GMCP now uses
  `hyper::body::{Bytes, Buf}`, the already-required Hyper API's public exports of the
  identical locked bytes types, rather than compiling a separate bytes facade.
  The external bytes implementation remains counted. The bytes facade still serves
  susi-server; the now-unreferenced flume facade remains a workspace member pending
  separate cleanup, so full-workspace builds still compile it. No capability is
  removed. Combined normal+build pillar reach stays at 46 workspace crates, while
  third-party names fall from 198 to 196 (flume and spin) and direct facade edges
  from 21 to 20. Both lower ceilings are pinned. This is a dependency-graph reduction,
  not a measured host build-time or total executable-size improvement. Standalone
  diagnostic PE/link-map evidence is separate from full production qualification;
  the source-bound GMCP evidence must be renewed in both phases before integration.

- 2026-11-10 T-DOTLATENCY-1 (measured test latency, no production deadline cut):
  move the model-manager unit suite from its production file into the existing
  excluded src/tests layout, preserving all cases and adding cancellation evidence.
  The gemi-models source ceiling falls from 16,737 to 15,938. This is test
  organization, not runtime code removal, dependency reduction or production
  qualification. Production retry sleeping remains unchanged; fixtures record
  the same backoff ticks while real HTTP, validation and durable publication run.
  A shorter explicit real-lock fixture budget preserves forward-progress and
  stalled-deadline assertions. Native Windows timing remains unverified.

- 2026-11-10 T-DOTWORKERLEASE-1 (external agent test organization):
  move the complete unchanged external unit-test module into the established
  same-crate `src/tests/` layout, retaining `external::unit_tests` paths and all
  assertions, including the new worker-lease regressions. The existing counting
  policy excludes that standalone test directory. Pin susi-gawd-agents from
  11,495 to 11,263 measured lines in the counted source directories. This is
  test organization, not a production-code reduction: the explicit-unlock lease
  guard remains unchanged, and no capability, test, or proof is removed. No build
  speed or executable-size improvement is claimed from the move.

- 2026-11-10 T-DOTSWARMFIXTURES20261110-1 (native Windows cancellation):
  lower the susi-gawd-swarm source ceiling from 20,673 to 20,641, paired with
  swarm commit 278b0d2504977d93d5f5e450284aa2acbf565382. The repair keeps the
  owned root alive until taskkill discovers its descendants, retains the direct
  child fallback, and removes the unscoped MSYS orphan sweep. Unix behavior is
  unchanged. The 32-line reduction is measured under the existing counting rules;
  no build-time or executable-size improvement is claimed. This owner-scoped
  ratchet depends on that swarm commit in the same serialized integration batch:
  the standalone latency worktree still has 20,673 swarm lines and cannot pass
  the reduced source guard until the swarm repair is composed. Combined-tree
  verification remains required; this record is not a gate or release receipt.

- 2026-11-10 T-DOTLINUXBYOKSAFE-1 (CEO-approved Kimi compatibility budget):
  the CEO approved adding only 12 lines to whichever current ceiling is lower.
  Preserve the latency reduction to 15,938 rather than main's older 16,737;
  the paired susi-gemi-models ceiling is therefore 15,950. Safe split commit
  aeea86295c2814ac86994cd7f419d3735469e702 adds exactly 12 counted lines in
  cloud.rs: four production lines remap retired Moonshot model identifiers,
  and eight inline test lines cover retired and current identifiers. No counting
  exclusions or other ceilings change. This ratchet depends on that exact safe
  split in the serialized combined gate. Credential handoff changes are excluded.
  Standalone owner source does not contain the split; prior standalone evidence
  is not proof that the combined candidate passes. Integration remains pending.
- 2026-11-11 (T-BUILDTIME-1, Linux x86_64, CEO laptop, under load: load
  average 7-53, other agents' rustc running): first Linux record of the
  budget, table "Linux x86_64" above. Cold `cargo build -p susi` 75.6-178.1 s
  wall (best runs 75.6, 76.0, 78.9 s), 395 units, so the 60 s budget is not met
  on Linux either. `[profile.dev] codegen-units` 256 -> 16 to match
  `[profile.test]`: the first `cargo test -p susi-gmcp` after a build
  recompiles 49 crates in 32.6 s instead of 162 in 78.2 s; the cold build
  measured no regression (4 of 5 alternated pairs faster, within load noise).
  No dependency or ceiling changes.
- 2026-11-11 (T-CORECUT-1, Linux x86_64, CEO laptop): the daemon's
  pillar-free host runtime (26 modules, 8,737 lines: supervision, watchdogs,
  NAT, TLS, port fallback, resource governor, OS provisioning, zc_* startup
  checks) moved unchanged into the new crate `susi-daemon-runtime`, which
  compiles beside the pillars; `susi-daemon` re-exports every module under its
  old path. No capability, port or command left; no pillar edge or ceiling
  moved. Three alternated cold pairs against `ed9710c17` (load 3-17): daemon
  start to binary done 6.9/7.3/8.1 s -> 5.9/6.1/6.6 s, the daemon unit
  3.9/4.2/4.8 s -> 2.9/3.2/3.5 s; whole-build wall 28.3/42.0/44.5 s base vs
  36.9/39.9/50.3 s change is inside the load noise and not claimed. The same
  runs give the first low-load Linux numbers (28.3 s at load 3), section
  "Linux x86_64".
- 2026-11-11 (T-CORECUT-2, Linux x86_64, CEO laptop): `susi-vendor-wasmtime`
  compiles wasmparser 0.259 and wasm-encoder 0.259 with `std` only; the cell
  facade uses just their core-module reader and encoder (memory cap before
  Wasmi validates), so the default validator, component model and serde
  support are no longer built. Components are still refused as cells (the
  version header parses without the component model). Three alternated cold
  pairs against `871425ffa` (load 5-14): wasmparser 0.259 4.7/7.8/7.9 s ->
  2.0/4.0/5.1 s, units 396 -> 386; whole-build wall 26.4/36.3/37.6 s vs
  34.5/36.4/36.3 s is inside the load noise and not claimed.
- 2026-11-11 (T-CORECUT-3, Linux x86_64, CEO laptop): the `susi` binary no
  longer depends on rayon. Its only use was a global pool built at startup
  that no code ran work on (GAWD moved to `susi_core::parallel` on
  2026-11-07), so the facade `susi-vendor-rayon` is deleted and rayon,
  rayon-core, crossbeam-deque and crossbeam-epoch leave the build (criterion
  keeps rayon as a benchmark dev-dependency only). Two alternated cold pairs
  against `4e49f290b`: units 386 -> 374; at load 1.6-5.7 wall 26.2 -> 25.9 s
  and unit CPU 224.5 -> 215.8 s; the second pair (load 7.6-9.3) is noise.
  The removed units cost about 4 CPU-seconds; no wall gain is claimed.
- 2026-11-11 (T-CLAUDEWASMSTD-1, Linux x86_64, 4-thread cloud container):
  the cell memory cap (`susi-vendor-wasmtime` `cell.rs`) rewrites the
  module's memory section with a std-only section walker (LEB128, the four
  memory-limit flags, custom page sizes, memory64 widths) instead of
  wasmparser 0.259 + wasm-encoder 0.259, which were a second copy beside
  Wasmi's own wasmparser 0.228. Same refusals (components, malformed
  sections, page-size exponent >= 64, initial size above the cap); three new
  tests pin the byte-exact rewrite, idempotence and the malformed cases.
  Units 374 -> 371, pillar third-party reach measured 191 (ceiling 196 ->
  191). Cold `cargo build -p susi` on 4 threads: 228.6 CPU-s / 63.3 s base
  `68e83f0d8`, 233.6 CPU-s / 64.9 s with the change: the removed units cost
  about 3 CPU-seconds, inside this machine's run-to-run noise, so no time gain
  is claimed.
- 2026-11-11 (T-CLAUDENOA2ASDK-1, Linux x86_64, 4-thread cloud container): the
  A2A SDK leaves core. `susi-gawd-a2a` serves the A2A subset the daemon
  exposes from its own `src/protocol` (wire types, JSON-RPC 2.0 on `/` with
  the 1.0 method names and the slash aliases, `message/stream` and
  `tasks/resubscribe` as SSE on `/stream` with the 15 s keep-alive, task
  get/list/cancel, push-notification configs and delivery over
  susi-http-transport, the same error codes and texts) on the
  axum/tokio/hyper stack already in core; the task store and its bounded
  retention are its own. A transcript of 79 real HTTP exchanges plus a push
  delivery, recorded from ra2a 0.10.1 before it left (`26bb86ee6`), passes
  unchanged against the in-tree code. One deliberate difference: an SSE
  stream now ends after its final event; ra2a held it (and a concurrency
  slot) open. `susi-vendor-ra2a` and the ra2a notice supplement are deleted;
  36 lock entries leave (ra2a, reqwest, hyper-tls, hyper-rustls, rustls,
  rustls-webpki, ring, encoding_rs, chrono, iana-time-zone, tower-http,
  uuid, mime_guess, unicase, windows-sys 0.52 and its target crates, ...);
  `cargo tree -p susi -i reqwest` finds nothing. No crate or third-party
  dependency is added. Pillar reach is unchanged (46 workspace, 191
  third-party, 20 facade edges: the A2A tier is outside the pillar closure
  since T-A2ASTD-1). Lines: susi-gawd-a2a 2,113 -> 3,702 (`src/protocol`
  1,744 lines in place of ra2a's 10,221), re-pinned under the T-NOMCPSDK-1
  rule. Cold `cargo build -p susi` on 4 threads with other agents' builds
  running (load 3-7): base `46758d8d8` 82.8 / 74.0 / 84.3 s wall, 298.8 /
  268.0 / 309.2 CPU-s, 371 units; with the change (`d94212915`, landed
  rebased as `91ab08900`) 75.7 / 64.4 s, 275.6 /
  226.7 CPU-s, 342 units. The ratchet is the unit count, 371 -> 342; in the
  third base run the 29 removed units cost 27.0 CPU-s (ra2a 12.6, reqwest
  4.8, encoding_rs 2.2, chrono 1.4, num-traits 1.2, ...) while susi-gawd-a2a
  grew 2.4 -> 3.0 s. The whole-build totals overlap across runs under this
  load, so no wall gain is claimed from them.
- 2026-11-11 (T-CLAUDEBUILDIDLE-1, Linux x86_64, 4-thread cloud container,
  idle: load 0.4, no other rustc): the day's cuts measured together. Cold
  `cargo build -p susi`, base `68e83f0d8` (before T-CLAUDEWASMSTD-1 and
  T-CLAUDENOA2ASDK-1): 63.3 s wall, 228.6 CPU-s (third-party 136.7), 374
  units. Main `19f37fd29`+: 61.7 / 61.3 s wall, 217.7 / 217.5 CPU-s
  (third-party 125.9 / 123.8), 342 units. About 11 CPU-seconds (5%) and 32
  units fewer; on 4 threads the build is CPU-bound (217 CPU-s / 4 = 54 s floor),
  so wall moves with CPU-seconds there, while on the 28-thread laptop the
  serial path (macro front, susi-config, susi-core, daemon, binary) sets it.
- 2026-11-11 (T-AGYCLOUD-3, Linux x86_64, 2-vCPU cloud VM): delete the
  unreferenced `crates/susi-vendor-flume` workspace facade left pending by
  T-DOTHARNESSNATIVE-1 (`Cargo.toml`, `Cargo.lock`, `VENDOR_FACADES` in
  `tests/architecture_tests.rs`). `susi-vendor-flume` and `flume 0.12.0` leave
  `Cargo.lock` and `--workspace` builds, dropping `flume`'s workspace feature
  unification on `fastrand`, `getrandom` (`js-sys` / `wasm-bindgen`), and `spin`
  (`lock_api`). Pillar reach ratchets from 191 to 184 third-party crates (46
  workspace crates and 20 direct facade edges unchanged; `cargo build -p susi`
  host unit count stays 342 on x86_64 Linux). Ratify accumulated source
  reductions in `LINE_CEILINGS` (`susi-gawd-swarm` 20,641 -> 20,635,
  `susi-gawd-agents` 11,263 -> 10,447, `susi-gemi` 21,290 -> 21,263,
  `susi-harness` 40,790 -> 40,594; -1,045 counted lines total). No host
  build-time or executable-size reduction is claimed.
- 2026-11-11 (T-AGYCLOUD-4, Linux x86_64, 2-vCPU cloud VM): delete two
  workspace facades (`crates/susi-vendor-futures` and `crates/susi-vendor-bytes`)
  and remove two pillar direct facade edges (`susi-gemi -> susi-vendor-parking-lot`,
  `susi-gawd-agents -> susi-vendor-regex`). `susi-harness` replaces its five
  `futures::stream::poll_fn` + `axum::body::Body::from_stream` sites (`cockpit`,
  `gateway`, `ingress`) with a 26-line `poll_body` over `hyper::body::Body`
  (`susi-vendor-hyper`, already in the pillar facade set via `susi-gmcp`);
  `susi-server` imports `hyper::body::Bytes` directly; `susi-gemi` (`local_plug`)
  uses `std::sync::RwLock`; and `susi-gawd-agents` (`accountability`) replaces
  its sole `Regex::new(r"\bFAILURE\b")` (and an `expect_used` allow) with a std
  word-boundary scan. Six lockfile packages leave (`susi-vendor-futures`,
  `susi-vendor-bytes`, `futures`, `futures-executor`, `futures-io`, proc-macro
  `futures-macro`), shrinking `cargo tree -p susi -e normal,build` unique
  packages from 348 to 342. Pillar ceilings ratchet on all four axes:
  `WORKSPACE_CEILING` 46 -> 45, `THIRD_PARTY_CEILING` 184 -> 180,
  `DIRECT_FACADE_EDGES_CEILING` 20 -> 19, and `LINE_CEILINGS` (`susi-gawd-agents`
  10,447 -> 10,443, `susi-gemi` 21,263 -> 21,171 with unchanged `usage_accounting`
  unit tests moved to `src/tests`, `susi-harness` 40,594 -> 40,575 with unchanged
  `MicroUsd` inline tests moved from `lib.rs` to `src/tests/money.rs`).
- 2026-11-11 (T-AGYCLOUD-5, Linux x86_64, 2-vCPU cloud VM): delete the two
  retired vendored patch trees (`vendor/reqwest` and `vendor/fluent-uri`,
  73 files / 28,435 lines / 1.05 MiB) left unused after `T-CLAUDENOA2ASDK-1`
  and the std JSON-schema cut, remove `[workspace.dependencies] reqwest` and
  the two `[patch.crates-io]` entries from `Cargo.toml` and `Cargo.lock`
  (`[[patch.unused]]`), and enforce zero `[[patch.unused]]` entries in
  `tests/architecture_tests.rs`.
- 2026-11-11 (T-NETTRANSPORT-1, Linux x86_64, CEO laptop, 28 threads, other
  agents compiling): the pillars' outbound client (`susi-http-transport`
  `http_call`, used by GEMI's BYOK providers, gemi-models downloads, GMCP,
  GAWD peers and the harness gateway) is HTTP/1.1 on std sockets
  (`wire.rs`, `connect.rs`) instead of ureq, over the platform TLS the pillars
  already link for their servers (native-tls: Schannel, platform OpenSSL,
  Security.framework) with the platform trust store; verification is never
  disabled. The ureq profile is kept: one total deadline over DNS, connect,
  TLS, redirects and the body; its redirect rules; ALL/HTTPS/HTTP_PROXY with
  NO_PROXY through CONNECT, loopback always direct. No `Accept-Encoding: gzip`
  is sent and connections are not pooled. `susi-vendor-ureq` is deleted;
  nothing is added. Pillar reach 180 -> 161 third-party, 45 -> 44 workspace
  (facade edges stay 19). Cold `cargo build -p susi -j 8`, alternated with
  base `bf0bb15a6` at load average 12-18: base 66.5 / 91.0 s, change 66.8 /
  80.6 s; units 267 -> 256 (ureq, ureq-proto, flate2, miniz_oxide, adler2,
  crc32fast, simd-adler32, der 0.8, pem-rfc7468 1.0, utf8-zero and the facade
  leave the host build). The wall numbers are load noise; no wall gain is
  claimed. Not done, with the reason: a pure-Rust TLS stack. rustls needs
  rustls-webpki and untrusted (ISC) and webpki-roots (CDLA-Permissive-2.0),
  which `deny.toml`'s strict licence list does not allow; its only offline
  providers are ring (C and assembly, ISC) and aws-lc-rs (C); and openssl
  would stay in the closure anyway, because GMCP, GEMI REST and A2A serve TLS
  through native-tls. tokio, hyper, h2 and mio stay: susi-gmcp, susi-gemi,
  susi-gawd and susi-harness depend on them directly (GMCP and harness
  servers).
- 2026-11-11 (T-SMALLFACADES-1, Linux x86_64, CEO laptop): three small
  facades leave core. `regex`: the truth gate's eleven static claim patterns
  (`susi-core` `truth.rs`, `verification.rs`) run on `susi_core::pattern`, a
  Pike VM for exactly the syntax they use that refuses anything else; checked
  against regex 1.13.1 on 15.6 million generated (pattern, text) pairs with no
  unexplained difference, plus 2.2 million on small edge patterns (lazy and
  greedy repetition, empty matches, `$`), frozen as table tests. One
  documented difference: `\b` counts alphanumerics and `_` as word chars
  (the rule T-AGYCLOUD-4 gave GAWD's FAILURE token), where Unicode `\w` also
  counts combining marks and joiners and not numbers outside `Nd`.
  `parking_lot`: the locks of susi-config, susi-gawd-swarm and susi-gmcp are
  `susi_paths::sync` (std-backed, never poisoning, same call sites).
  `dashmap`: GMCP's custom tool maps, the tool registry and the daemon
  blackboard are std maps behind those locks; a custom handler runs with no
  lock held. `susi-vendor-regex`, `susi-vendor-dashmap` and
  `susi-vendor-parking-lot` are deleted (regex stays in the workspace only as
  criterion's dev-dependency, parking_lot as tokio's). Pillar reach 161 -> 154
  third-party (aho-corasick, crossbeam-utils, dashmap, regex, regex-automata,
  regex-syntax, susi-os-unicode), 44 -> 41 workspace, 19 -> 17 direct facade
  edges; `cargo tree -p susi` host packages 250 -> 243. Pillar line counts
  unchanged. No build-time gain is claimed.
- 2026-11-11 (T-HARNESSSTD-1, Linux x86_64, CEO laptop, other agents'
  builds running): axum leaves susi-harness. The metering gateway (the BYOK
  OpenAI-compatible port, buffered and SSE-streamed answers), the public
  ingress and the harness API are served by `crates/susi-harness/src/http.rs`,
  a std, blocking, thread-per-connection HTTP/1.1 server whose handlers stay
  async on the caller's Tokio runtime. A transcript of 69 raw exchanges
  recorded from the axum 0.8.9 / hyper 1.12 stack first (`bfd0a8eea`:
  routing, 404/405 + allow, HEAD, keep-alive and close on 1.0 and 1.1, 400
  and 431 heads, chunked request bodies, 100-continue, the 2 MiB body limit
  and its text, buffered and streamed completions chunk by chunk, the
  ingress refusals, the harness API's guarded command route with axum's
  415/400/422 JSON rejections and their paths, the first SSE event) passes
  byte for byte against the std server. Kept as they were: the body limits
  (2 MiB; 64 KiB for operator commands), no server-side read timeout,
  authentication, loopback binding, handler drop when a client hangs up,
  graceful stop (in-flight calls finish, idle connections close). Not
  served any more: HTTP/2 prior knowledge, which axum's `http2` feature
  accepted. Live: `harness-gateway` on a free loopback port against a local
  stand-in provider answered `/health`, a buffered and a streamed
  completion (chunks arrived 0.3 s apart as the provider sent them, the row
  carried the provider's usage), the ingress 401 and an admitted call
  booked under its account, and a SIGTERM during a stream finished the
  stream and its row before the process exited (0.54 s). Pillar reach
  154 -> 145 third-party (axum, axum-core, matchit, tower,
  serde_path_to_error, serde_urlencoded, sync_wrapper, mime, ryu), 41 -> 40
  workspace, 17 -> 16 direct facade edges; susi-gawd-a2a and
  susi-leaf-services still use axum, so `cargo tree -p susi` host packages
  stay 243 and no host build-time gain is claimed (`cargo tree -p
  susi-harness`: 120 -> 107 packages). Lines: susi-harness 40,575 -> 39,446;
  production code grew by 1,859 (the server, 1,843 lines, in place of the
  nine crates' ~45,000 source lines) and the inline test modules of the
  gateway, ingress and harness API (2,988 lines) moved unchanged to `src/tests`.
- 2026-11-11 (T-SMALLFACADES-2, Linux x86_64, CEO laptop): the `url` facade
  leaves core. `susi_paths::weburl::WebUrl` parses `http(s)` URLs in the form
  a WHATWG parser keeps (scheme and host lowercased, default port and port
  zeros dropped, empty path `/`, canonical IPv6) and refuses every URL it would
  rewrite otherwise (whitespace, non-ASCII, `\`, userinfo, `%` or `xn--` in
  the host, IPv4 not written as four plain decimals, dot segments, chars it
  would percent-encode); `is_absolute_uri` answers `url::Url::parse(..).is_ok()`
  for every scheme except special-scheme hosts that need IDNA tables, which it
  refuses. Against url 2.5.8: 79,306 accepted URLs with no difference in any
  part or in origin equality, and 800,000 validity checks with no difference
  besides those refusals. GEMI's model download name and origin checks and
  GMCP's content URIs and SSRF-guarded browse URL use it; `susi-vendor-url` is
  deleted. Pillar reach 145 -> 141 third-party (url, idna, utf8_iter,
  form_urlencoded), 40 -> 39 workspace, 16 -> 15 direct facade edges; `cargo
  tree -p susi` host packages 243 -> 239; susi-gmcp 25,900 -> 25,891 and
  susi-gemi-models 15,950 -> 15,945 lines. Error texts for refused URLs now
  name the reason. No build-time gain is claimed.
- 2026-11-11 (T-CARBONLEAN-1, Linux x86_64, CEO laptop): three workspace
  facades leave core (`susi-vendor-console-subscriber`,
  `susi-vendor-criterion`, `susi-vendor-sha1`), removing 63 packages (60
  third-party crates including `tonic`, `prost`, `console-subscriber`,
  `criterion`, `rayon`, `plotters`, `ciborium`, `flate2`, `zlib-rs`,
  `wasm-bindgen`, `regex` and `sha1`, plus the three facades) from `Cargo.lock`
  and `--workspace --all-targets` builds. `susi-daemon-runtime` (`nat.rs`
  RFC 5389 STUN `MESSAGE-INTEGRITY`) and `xtask` (`upstream_notices.rs` Git
  object SHA-1) run on inline pure-std RFC 3174 SHA-1 verified against RFC 3174
  and RFC 2202 HMAC-SHA1 vectors (`cargo tree -p susi` host packages 239 ->
  237). `crates/susi-core/benches/evidence_verify.rs` runs on
  `std::time::Instant` and `std::hint::black_box` (`harness = false` kept;
  935 ns/iter signature, 668 ns/iter truth verify). Lines: `susi-gawd-swarm`
  20,635 -> 20,116 (unchanged `amas.rs` inline unit suite, 519 lines, moved to
  `src/tests/amas.rs`, not a runtime cut).
- 2026-11-11 (T-CODEXAXUMOUT-1, Linux x86_64, CEO laptop): the remaining
  A2A and leaf-service HTTP listeners leave the axum/tower facades and use the
  shared std HTTP/1.1 server in `susi-http-transport`. The A2A and leaf
  contract suites stay green (52 + 39 + 43 tests, zero failures), including
  their wire and authorization behaviour; the old axum stack remains only in
  the recorded wire transcripts and compatibility comments. The two workspace
  facades (`susi-vendor-axum`, `susi-vendor-tower`) and ten registry packages
  (`axum`, `axum-core`, `form_urlencoded`, `matchit`, `mime`, `ryu`,
  `serde_path_to_error`, `serde_urlencoded`, `sync_wrapper`, `tower`) leave
  `Cargo.lock`; `tower-service` remains because GMCP still uses it. The
  four-pillar reach is unchanged at 39 workspace crates, 141 third-party
  crates and 15 direct facade edges because these listeners are outside the
  pillar closure. The platform TLS identity now exposes both its async and
  blocking accept paths, so the std server does not bypass certificate or
  protocol validation. No commercial source moved into core.
- 2026-11-11 (T-CARBONLEAN-2, Linux x86_64, CEO laptop): two more workspace
  facades leave core (`susi-vendor-shlex` and `susi-vendor-tracing-appender`),
  removing 9 packages from `Cargo.lock` (`crossbeam-channel`, `crossbeam-utils`,
  `symlink`, `thiserror`, proc-macros `thiserror-impl` and `time-macros`,
  `tracing-appender`, `susi-vendor-shlex`, `susi-vendor-tracing-appender`) and
  lowering `cargo tree -p susi` host packages 237 -> 229 (`-e normal`: 220).
  `susi_paths::shlex::split` provides `#![forbid(unsafe_code)]` POSIX shell word
  splitting verified with zero mismatches across 500,000 generated inputs
  against `shlex` before removal (`susi-gawd-agents` `safety.rs` and `susi-gmcp`
  `tools/core.rs` switch to it; `shlex 1.3.0` stays only in `cc`'s build edge,
  so pillar workspace ceiling drops 39 -> 38, direct facade edges drop 15 -> 14,
  and pillar third-party stays 141). `src/audit_writer.rs` provides a bounded
  (4,096-record) std `sync_channel` daily UTC rolling JSON audit writer with
  7-file retention and graceful flush-on-drop. Lines: `susi-gmcp` 25,891 ->
  25,889. GMCP requirement and `--mastery` evidence re-recorded in both phases.
- 2026-11-11 (T-CARBONLEAN-3, Linux x86_64, CEO laptop): two more workspace
  facades leave core (`susi-vendor-anyhow` and `susi-vendor-proptest`), removing
  25 packages from `Cargo.lock` (23 third-party crates: `anyhow`, `autocfg`,
  `bit-set`, `bit-vec`, `borsh`, proc-macro `borsh-derive`, `cfg_aliases`,
  `chacha20`, `core_detect`, `num-traits`, `proc-macro-crate`, `proptest`,
  `quick-error`, `rand`, `rand_core`, `rand_xorshift`, `rusty-fork`,
  `toml_datetime`, `toml_edit`, `toml_parser`, `unarray`, `wait-timeout`,
  `winnow`, plus the two facades) and lowering `cargo tree -p susi` host
  packages 229 -> 227 (297 -> 294 host build units). `src/anyhow.rs` provides
  `#![forbid(unsafe_code)]` pure-std error chaining (`Error`, `Result`,
  `Context` on `Result` and `Option`, `anyhow!`, `bail!`) for the `susi` CLI;
  `susi-abi` (`router.rs`, `swarm.rs`, `wire.rs`) and `susi-core`
  (`service_table.rs`) run deterministic `SplitMix64` property generators
  (1,024-4,096 generated inputs per property plus boundary vectors) in place of
  `proptest!`. Unused dev-dependencies on `susi-vendor-proptest` leave
  `susi-error` and `susi-gemi-models`. Clean-target offline cold `cargo build
  --offline --locked -p susi` (`CARBON_JOBS=24`) measured 29.36 s on Linux
  x86_64; pillar reach stays 38 workspace, 141 third-party, 14 direct facade
  edges (both removed facades sat outside the four-pillar normal closure).
- 2026-11-11 (T-AGYCLOUD-8, Linux x86_64, 2-vCPU cloud VM): the
  `susi-vendor-tower-service` facade leaves core. `susi-gmcp` (`server.rs`,
  `protocol_tests.rs`) calls `StreamableHttpService::handle` directly without
  boxing every HTTP request into a `tower_service::Service` trait wrapper in
  `mcp/http.rs`. Pillar reach: workspace crates 38 -> 37, direct facade edges
  14 -> 13 (`cargo tree -p susi` host packages 227 -> 226; third-party metadata
  reach stays 141 because `hyper-util` still depends on `tower-service`). Lines:
  `susi-gmcp` 25,889 -> 25,860 (-29 shipped lines). GMCP requirement and
  `--mastery` evidence re-recorded in both phases. No host build-time gain is
  claimed.
- 2026-11-11 (T-CARBONLEAN-4, Linux x86_64, CEO laptop): the
  `susi-vendor-tracing-subscriber` facade leaves core, removing 14 packages
  from `Cargo.lock` (13 third-party crates: `aho-corasick`, `lazy_static`,
  `matchers`, `nu-ansi-term`, `regex-automata`, `regex-syntax`, `sharded-slab`,
  `susi-os-unicode`, `thread_local`, `tracing-log`, `tracing-serde`,
  `tracing-subscriber`, `valuable`, plus `susi-vendor-tracing-subscriber`; 111
  total `Cargo.lock` packages removed across T-CARBONLEAN-1..4) and lowering
  `cargo tree -p susi` Linux host packages 214 -> 202 (`-e normal`: 205 -> 193;
  host build units 275 -> 263 atop T-CODEXAXUMOUT-1). `[patch.crates-io]
  regex-syntax` leaves `Cargo.toml` because nothing in `Cargo.lock` resolves
  `regex-syntax` any longer. `src/tracing_subscriber.rs` provides a
  `#![forbid(unsafe_code)]` pure-std `tracing::Subscriber` preserving
  `RUST_LOG` target-prefix directive filtering, structured single-line JSON
  audit records (with active `span`/`spans`) to `audit_writer`, and `stderr`
  console formatting. Pillar third-party reach ratchets 141 -> 140
  (`valuable`).
- 2026-11-11 (T-CARBONLEAN-5, Linux x86_64, CEO laptop): the
  `susi-vendor-http-body-util` facade and `http-body-util` leave core and
  `Cargo.lock` (-2 packages; 113 cumulative `Cargo.lock` packages removed across
  T-CARBONLEAN-1..5; Linux host packages 202 -> 200, `-e normal` 193 -> 191,
  host build units 263 -> 261). `susi_http_transport::http_body` provides
  `#![forbid(unsafe_code)]` shared Hyper `Body` combinators (`BoxBody`, `Full`,
  `Empty`, `Limited`, `LengthLimitError`, `Collected`, `BodyExt`) for
  `susi-gmcp` and `susi-server`. All three pillar reach ceilings ratchet down
  simultaneously: third-party 140 -> 139, workspace 37 -> 36, direct facade
  edges 13 -> 12 (`susi-gmcp` shipped lines unchanged at 25,860). GMCP
  requirement and `--mastery` evidence re-recorded in both phases.
- 2026-11-11 (T-CARBONLEAN-6, Linux x86_64, CEO laptop): the `susi-vendor-hex`
  facade and `hex` leave core and `Cargo.lock` (-2 packages; 115 cumulative
  `Cargo.lock` packages removed across T-CARBONLEAN-1..6; Linux host packages
  200 -> 198, `-e normal` 191 -> 189, host build units 261 -> 259, crossing
  below 200 host packages). `susi_paths::hex` provides `#![forbid(unsafe_code)]`
  pure-std lowercase hex encoding and case-insensitive decoding (`encode`,
  `decode`, `FromHexError`) verified with zero mismatches across 500,000
  generated inputs (250,000 `encode` + 250,000 `decode`) against `hex 0.4.3`
  before removal. All three pillar reach ceilings ratchet down simultaneously
  for the second consecutive wave: third-party 139 -> 138, workspace 36 -> 35,
  direct facade edges 12 -> 11 (`susi-gmcp` shipped lines unchanged at 25,860).
  GMCP requirement and `--mastery` evidence re-recorded in both phases.
- 2026-11-11 (T-CARBONLEAN-7, Linux x86_64, CEO laptop): the
  `susi-vendor-hyper-util` facade leaves core, removing 11 packages from
  `Cargo.lock` (10 third-party crates: `core-foundation`, `futures-task`,
  `futures-util`, `hyper-util`, `ipnet`, `percent-encoding`,
  `system-configuration`, `system-configuration-sys`, `tower-layer`,
  `tower-service`, plus `susi-vendor-hyper-util`; 126 cumulative `Cargo.lock`
  packages removed across T-CARBONLEAN-1..7; Linux host packages 198 -> 190,
  `-e normal` 189 -> 181). `susi_http_transport::http_conn` provides
  `#![forbid(unsafe_code)]` `TokioIo`, `TokioExecutor`, `TokioTimer`, and the
  24-octet `RFC 9113 §3.4` HTTP/2-preface-sniffing HTTP/1.1 + HTTP/2 connection
  `Builder` with slowloris `header_read_timeout` enforcement for `susi-gmcp` and
  `susi-server`. All three pillar reach ceilings ratchet down simultaneously for
  the third consecutive wave: third-party 138 -> 128 (-10 crates), workspace
  35 -> 34, direct facade edges 11 -> 10 (`susi-gmcp` shipped lines unchanged at
  25,860). GMCP requirement and `--mastery` evidence re-recorded in both phases.
- 2026-11-11 (T-AGYWIN-1, Windows x86_64, CEO workstation): `susi-vendor-tracing`
  leaves the four-pillar workspace closure across `susi-core`, `susi-gemi` and
  `susi-sandbox-client`, replacing the remaining diagnostic macros in those three
  crates with direct `std::io::stderr` reporting (`eprintln!`). Pillar workspace
  reach ratchets 34 -> 33 and direct facade edges ratchet 10 -> 9 (`tracing` and
  `tracing-core` remain in third-party reach only through Hyper/H2 internal spans
  until the std HTTP cut), while `LINE_CEILINGS` ratify the current shipped line
  reductions across `susi-gawd-a2a` (3,702 -> 3,619), `susi-gemi` (21,171 ->
  21,145) and `susi-harness` (39,446 -> 37,604).
- 2026-11-11 (T-CARBONLEAN-8, Linux x86_64, CEO laptop): external `tempfile`
  leaves core, removing 4 third-party packages from `Cargo.lock` (`fastrand`,
  `linux-raw-sys`, `rustix`, and `tempfile`; 130 cumulative `Cargo.lock`
  packages removed across T-CARBONLEAN-1..8). `susi-vendor-tempfile` becomes a
  zero-dependency `#![forbid(unsafe_code)]` pure-std leaf (`TempDir`, `Builder`,
  `tempdir`, `tempdir_in`) backed by atomic `std::fs::DirBuilder` (`0o700` on
  Unix) and `RandomState` + PID + timestamp + `AtomicU64` token generation with
  read-only cleanup fallback, and `vendor/native-tls` replaces `tempfile` in
  `src/imp/security_framework.rs` and `src/test.rs` with standard-library
  exclusive directory guards while keeping Apple/Windows/Linux TLS portability
  intact. Pillar third-party reach drops 4 crates (`THIRD_PARTY_CEILING`
  128 -> 124).


