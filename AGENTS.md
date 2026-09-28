# Agent Engineering Mandates

Hard rules for any code written in this repo — human or AI-generated. These
are enforced mechanically where possible; the rest are reviewed in CI.

## Enforced (compiler — violations fail the build)

- **`unsafe` is forbidden or denied in every crate.**
  - `#![forbid(unsafe_code)]`: susi-abi, susi-adapters-llm, susi-core,
    susi-dsh-cell, susi-error, susi-gawd, susi-gawd-agents, susi-gmcp,
    susi-http-transport, susi-leaf-services, susi-native-client, susi-paths,
    susi-sandbox, susi-sandbox-client, susi-server, susi-universal-cell,
    susi-vendor-agents, susi-vendor-candle, susi-vendor-chrome,
    susi-vendor-cloud, susi-vendor-fastembed, susi-vendor-mcp,
    susi-vendor-mcp-server, susi-vendor-models, susi-vendor-syn,
    susi-vendor-tantivy, susi-vendor-wasmer, susi-vendor-web, xtask, root
    package.
  - `#![deny(unsafe_code)]` + per-function `#[allow(unsafe_code)]` with a
    `// SAFETY:` justification: susi-agents, susi-config, susi-daemon,
    susi-gawd-a2a, susi-gawd-swarm, susi-gemi, susi-gemi-models, susi-tools.
    Justified sites are FFI only (libc syscalls, candle mmap, env mutation in
    tests).
- **No panic-path macros in production code.** Workspace clippy denies:
  `unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`,
  `unreachable`. Every crate has
  `#![cfg_attr(test, allow(...))]` so tests may still unwrap/panic.
- **Exhaustive enum matching**: `clippy::wildcard_enum_match_arm = deny`.
  Catch-alls that are intentional (e.g. `bail!("not a cloud adapter")`) carry
  `#[allow]` + a justification comment.
- `unsafe_op_in_unsafe_fn = deny` workspace-wide.
- `rustfmt` is enforced (`cargo fmt --all --check` in CI and pre-commit).

## The audit-trail pattern

When a lint is denied, exceptions are inline and auditable:

```rust
#[allow(clippy::expect_used)]  // why this invariant can never fail
fn bundled_defaults() -> ...   // include_str! asset — parse failure is a
                               // build bug, not a runtime condition
```

An `#[allow]` without a written justification is a compliance violation.
`tests/architecture_tests.rs` (`every_panic_path_allow_is_justified`) enforces
this for the panic-path lints.

## Writing code here

- Return typed `Result<T, EaiError>` from anything touching I/O, agent state,
  or IPC. Constructors: `EaiError::{sandbox, config, io, network, ...}`.
- Clock timestamps: `duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)`.
- Mutex poisoning: `.lock().unwrap_or_else(|e| e.into_inner())`.
- Prefer infallible constructors over `parse().unwrap()`
  (e.g. `SocketAddr::new(...)` not `"127.0.0.1:80".parse().unwrap()`).
- No new inter-crate edges without necessity; leaf order is
  `paths → error → config → core/sandbox → services → daemon`.
- **susi-* crates declare zero third-party dependencies** — normal, build,
  dev, and target-scoped sections alike. Every crates.io/git crate is owned
  by exactly one `susi-vendor-*` crate: an implementation vendor
  (`susi-vendor-candle`, `susi-vendor-wasmer`, `susi-vendor-mcp*`, …) or a
  pure re-export facade (`susi-vendor-serde`, `susi-vendor-tokio`, …).
  Consumers write `serde = { package = "susi-vendor-serde", path = "../susi-vendor-serde" }`
  so `use serde::…` is unchanged. A new external crate means a new facade —
  `susi_crates_declare_zero_external_dependencies` (architecture_tests)
  enforces it.
- **Vendor *code* lives in vendor crates, not only vendor deps.** Any code
  that speaks a third-party agent, model-provider, or web API must live in a
  `susi-vendor-*` crate: `susi-vendor-agents` (external agent executors +
  framework adapters), `susi-vendor-models` (cloud/frontier/OpenRouter/
  open-weight/HF discovery), `susi-vendor-web` (Open-Meteo, DuckDuckGo),
  `susi-vendor-cloud` (kubectl/docker/aws/gcloud/az CLIs). susi crates
  consume them through re-exports; no third-party endpoint strings or
  vendor protocol logic in non-vendor crates.
- `susi_http_transport::http_call*` returns non-2xx responses; callers must
  check status. `HttpCall::into_bytes(max)` reads `max + 1` and rejects overflow.
  The std-only `susi_paths::loopback` client caps complete responses at 16 MiB.
- Do not redeclare normal dependencies under dev/build scopes unless extra
  test-only or build-only features are required.
- Standalone leaf-service binaries enable only their `service-*` feature;
  root `service-run` may enable `all-services`.
- No stubs: no `todo!()`, `unimplemented!()`, or dead `pub` surfaces left
  "for later".
- **Mandate 48 (Self-Build Order).** The local susi (`~/.susi/bin/susi` + its daemon) is release-only: it builds
  the next susi. Dev builds (`cargo xb`, `target/` binaries) must never
  install there; `scripts/susi-release-sync.sh` is the only path in.
  Dev binaries run as their own instance (`~/.susi-dev`, ports 9190–9194;
  `src/dev_instance.rs`) — verify dev features there, not on the release daemon.

## Test policy

- Property-based testing with `proptest` for state machines and pure
  primitives (see `susi-error::redact::prop_tests`).
- Integration tests live in `tests/`; they carry the panic-capable-macro
  exemption header already.

## Verify before pushing

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

## Multi-agent parallel work

Several agents work concurrently in their own git worktrees on their own
branches, then merge to local `main` and push. These rules keep that
convergent:

- **Evidence IDs are namespaced per agent.** Mint entries as
  `EV-DEVIN-<n>`, `EV-CURSOR-<n>`, `EV-CLAUDE-<n>` (per-agent monotonic).
  Date-stamped `EV-YYYYMMDD-NNN` IDs are the legacy scheme — do not mint
  new ones, since independent counters collide on merge (this forced two
  renumbering passes). A uniqueness check in
  `crates/susi-gawd-agents/build.rs` makes a collision a build failure.
- **On evidence/identity/README merge conflicts, union entries — never
  delete or renumber another agent's entries.** Resolve JSON conflicts by
  keeping both sides' entries, then validate with `jq empty`.
- **Always `git fetch` + merge `origin/main` before pushing.** Pushes to
  `main` must be fast-forward; compile (`cargo check --workspace`) before
  pushing a merge so fixup commits never ship an uncompiled merge.
- **CI is branch-scoped and affected-crate-scoped.** Feature-branch pushes
  run fmt + cargo deny + `cargo check` on just the crates the diff touches
  (`scripts/ci-changed-crates.sh`; workspace-wide inputs and root-package
  changes fall back to a full check). On `main`, PRs, and manual dispatch
  the suite runs as six parallel nextest shards (foundation, daemon, gawd,
  gemi, vendor-cells, root-cli) plus a lint job and the live susi-native
  e2e job — the gate is the slowest shard, not one serial workspace build.
  The rolling dev release builds only on `workflow_dispatch` or a
  head-commit subject that starts with `[dev-release]`.

## Ratchet items (not yet at zero — do not regress)

- `clippy::pedantic` + `clippy::nursery`: ~2.4k warnings baseline; new code
  should be pedantic-clean even though the lint isn't denied workspace-wide.
- `Mutex`/`RwLock` in hot paths: prefer bounded `flume`/tokio channels for new
  work; existing locks are being migrated incrementally.
- Unwired `susi-daemon` modules: 0 of 56 (`tests/architecture_tests.rs`
  `unreachable_daemon_modules_only_decrease` now asserts none). A new module
  must be reached by real production wiring; discarded probes and type-only
  references do not count.
- Formal verification (`kani`) and `cargo-geiger` unsafe-tree auditing are
  roadmap items — see `.agents/roadmap.json`.

## Autonomous Self-Development (SUSI Building SUSI)

This repository is structurally designed for SUSI to act as the primary intelligence layer for its own development.

- **Primary Mode (Agentic Resolution)**: When operating on GitHub issues or PRs (e.g., via `.github/workflows/susi-builder.yml`), the SUSI binary MUST use native tools to edit code, execute `cargo` / `./build-gpu.sh` commands, and test itself.
- **Delegation Protocol (A2A)**: If a capability gap prevents SUSI from directly building a feature (or an ultimate fallback is triggered), it MUST leverage the A2A protocol to invoke compliant external agents (e.g., `antigravity`, `cursor`, `aider`). 
  - Delegations are written to the `~/.susi/delegations/` ingress directory.
  - External agents invoked this way are bound to the exact same strict mandates listed in this file.
- **Self-Build Order (Mandate 48)**: SUSI, its delegates, and human developers all build dev in `target/`, verify on the dev instance (`~/.susi-dev`, ports 9190–9194), and change the installed susi only by cutting a release (`susi release`), which `scripts/susi-release-sync.sh` promotes. Never install or hot-swap a dev binary into `~/.susi/bin`. SUSI states this contract in every task it gives a coding agent in its own tree (`susi_core::self_build::BRIEF`: delegated external agents and DAG shell nodes), self-patches clear the full fmt/clippy/test gate, and the CI builder (`susi-builder.yml`) is the *released* susi, landing work as a PR (draft if the gate fails).
- **Continuous Execution**: The `exec_command` native tool explicitly permits `cargo`, `gh`, `bash`, and `sh` to allow SUSI to test itself and manage source control natively without triggering governance violations.
