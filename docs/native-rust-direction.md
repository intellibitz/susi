# Native Rust Susi: implementation contract and audit

Owner instruction (2026-11-01): Susi is agent-built. The CEO does not inspect code. Product requirements are Rust-only implementation, native Windows builds, isolated parallel agents, local debug and production builds, no GitHub dependency for development, Apache-2.0/BSD/MIT only, minimal footprint, and automatic configuration.

## Product proposition

**Susi: AI that earns its next upgrade.**

A local-and-cloud AI operating environment that can improve itself, but promotes a candidate only when reproducible evidence shows that it satisfies its goal and does not regress correctness, resource use, or isolation. Keep the previous verified version available for rollback. Model choice should follow measured task outcomes and budgets. This is the intended differentiator, not a claim of market uniqueness or a claim that recursive self-improvement is already complete.

## Working implementation

- `cargo local-build debug` and `cargo local-build production` invoke native Cargo directly. They select the compiler host (MSVC on Windows), a private worktree target, explicit profiles, and the Susi executable. They do not invoke setup shell scripts, autodetect CUDA, publish, or install over production. Production candidate builds first require the strict local license audit to pass; the current dependency graph is blocked.
- `cargo local-workflow start worker-a --scope crates/susi-paths` creates a local branch/worktree and reserves its scope. `commit worker-a "message"` checks all staged, unstaged and untracked changes against ownership before a local commit, without invoking legacy remote hooks. This is a scope gate, not a test attestation. `list` reports reservations; `release worker-a` releases only a clean workspace's reservation and preserves its source and branch.
- The launcher rejects overlapping reservations, including case differences and parent/child paths. An atomic local lock serializes reservations. Corrupt or abandoned state fails closed. Reservations are cooperative: agents can still write outside them. This is not yet an enforced integration queue. The outer Cargo invocation that bootstraps xtask can still inherit Cargo target settings; private-output checks apply to the inner Susi build.
- Patched reqwest and esaxx-rs sources ship under `vendor/`, with original licenses and recorded provenance. The lockfile no longer resolves either through GitHub. Crates.io sources and the Rust/MSVC toolchain must still be provisioned. Cargo offline mode does not sandbox arbitrary build scripts or remove third-party runtime downloads.
- The license allowlist is Apache-2.0, MIT, BSD-2-Clause and BSD-3-Clause. Other licenses are no longer silently exempted. A failing audit means migration is incomplete; it is not a compliant release.
- Tokenizers now keeps its existing Oniguruma and progress-bar features but disables esaxx_fast; its training suffix-array implementation uses Rust instead of C++. The Windows feature graph verifies the cpp/cc feature path is absent. This does not remove Oniguruma C. Unigram training performance must be remeasured if that capability is used.
- An unused harness-to-Tantivy dependency edge was removed. Tantivy remains reachable elsewhere. This is not evidence of an application-size reduction.

## Audit baseline

Source baseline: locally available commit `3eb12285cb06294d8c3b765c84757940667e7bdf`, version 0.21.38. The selected primary checkout on F: was version 0.21.29, with an existing evidence edit; it was preserved. Implementation is in a separate worktree.

Windows metadata before changes reported 88 workspace packages and 680 packages in the metadata set. These are not compiled-unit counts. The strict license audit after policy tightening reported 63 rejected entries across the configured platform graph.

Confirmed baseline default native-code dependencies (the esaxx C++ path is removed by this change): ONNX Runtime (ort-sys/fastembed), Oniguruma (onig_sys/tokenizers), esaxx C++, zstd C, and ring native cryptography. Root no-default-features does not disable unconditional or downstream-default dependencies.

Confirmed license blockers include ISC-only dependencies; Unicode-3.0 dependencies; Zlib-only dependencies; CDLA-Permissive-2.0 certificate roots; ring's Apache-2.0 AND ISC; unicode-ident's (MIT OR Apache-2.0) AND Unicode-3.0; Wasmtime/Cranelift's Apache-2.0 WITH LLVM-exception. Their licenses must not be rewritten or misrepresented. Toolchain and Windows platform licenses must be distinguished from the Susi-distributed ecosystem.

The harness memory module returns an embedding but currently does not persist or retrieve stored records. It is not evidence of durable learning.

## Remaining acceptance work

1. Replace native-code and disallowed-license dependency paths while preserving their tested behavior. Audit the complete resolved graph, build scripts, native sources, bundled data/models and notices. A Rust facade around C/C++ does not count as Rust-only.
2. Reduce GMCP's default rich-tool graph and Wasmtime's default feature graph based on real capability use; preserve fuel, memory limits and WASI behavior. Benchmark before/after cold and warm builds, executable size, idle memory and request latency.
3. Enforce reserved edit scopes and add a serialized local integration queue that tests the actual combined tree before promotion. Local Git is permitted; GitHub must be optional. Cross-host coordination needs a shared coordinator, not independent filesystem claims.
4. Isolate each running agent's state, ports and scratch home as well as its source/build directories. Existing development runtime defaults share .susi-dev; separate worktrees alone do not isolate running daemons.
5. Prove debug and optimized builds, hermetic tests, executable smoke checks, license checks and rollback. An optimized build is a candidate, not an approved production release.
6. Deliver default discovery and operating choices without user-written configuration. Credentials and paid-service authorization remain explicit. Measure first-run behavior on a clean Windows machine with no WSL, Bash, Python or Node requirement.

Do not claim all ten owner requirements complete until these acceptance checks pass. Keep incomplete tasks open and evidence visible; agents own implementation and verification.
## Verification on 2026-11-01

- Final native MSVC debug build passed; `susi.exe --version` reported 0.21.38 and `--help` exited successfully in an isolated scratch home.
- 26 xtask binary unit tests passed on the integrated tooling, including concurrent scope reservation, guarded commits and Windows path handling. The existing xtask integration suite also passed before the final dependency-feature change. Clippy with warnings denied and workspace formatting passed for the tooling changes.
- The vendor-source contract test passed. The pretrained-tokenizer golden test passed, compiled directly against the rebuilt tokenizers library and host dependencies; this is not a claim that the entire Candle test suite ran.
- The final locked Windows feature graph contains no esaxx_fast, esaxx-rs/cpp or esaxx-rs/cc feature path.
- The strict license audit still fails with 63 rejected entries. The production command was exercised and correctly refused to proceed.
- Full workspace regression tests, complete inference qualification, production artifacts, rollback and clean-machine provisioning remain unverified. No production installation was replaced.
- Local logs, executable digest and feature-graph evidence are under `.artifacts/native-audit/` (intentionally untracked build evidence).

## Integrated migration progress (supersedes the earlier implementation snapshot)

The older verification section records the initial baseline, not qualification
of subsequent edits. As of the second implementation pass:

- CEO mandates 62–71 and the canonical agent contract govern all workers.
  CLAUDE.md, GEMINI.md and Copilot instructions point to that contract; obsolete
  automatic shell startup hooks were removed from .claude and .codex.
- The local workflow now merges into an isolated candidate, runs local gates on
  the combined tree, and publishes the integration ref atomically. Tests cover
  conflicting scopes, failed gates, source changes and competing publication.
- Embeddings use Rust Candle BERT with pinned MIT BGE weights. A real-model
  comparison against the former ONNX engine found cosine similarity at least
  0.999999999999784 and maximum absolute component difference below 1.3e-7 for
  the three recorded inputs. This is numerical evidence for those inputs, not
  an exhaustive model-equivalence claim. Tokenizers uses fancy-regex, removing
  the default Oniguruma native path.
- The sandbox uses Wasmi, with tested fuel/memory limits and a bounded WASI host.
  A compiled Rust WASI program exercises arguments, empty environment, denied
  host files, clocks, polling, output and successful exit. Unsupported host
  operations return errors. See its crate README for limits and compatibility.
- Local patches replace default foldhash, zlib-rs, Candle yoke ownership and
  Tantivy's C Zstandard codec; vendor/README.md records exact changes.
- Development instances now allocate private runtime directories and distinct
  cooperative port ranges per executable directory, without copying production
  credentials. Explicit SUSI_HOME remains an intentional caller override.

Strict license qualification, the final integrated executable, production
promotion and full installed first-goal behavior remain acceptance gates. Do not
reuse the earlier debug artifact or its digest as evidence for this source tree.
