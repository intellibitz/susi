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
| 0 | third-party crates compiled | 331 | 313 |
| 0 | workspace crates compiled | 59 | 58 |
| 0 | vendor facades named directly by the pillar crates | 26 | 26 |
| 0 | Rust lines, pillar families (gawd 4, gemi 2, gmcp, harness) | 180,441 | 171,558 |
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
