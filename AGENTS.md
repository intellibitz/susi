**START HERE: read the CEO mandates below, inspect local state, and reserve an isolated worktree before editing.**

# CEO vision and governing contract

The CEO never looks at code. Susi is an RSI AI OS for the local and cloud AI
ecosystem, built entirely by agents. Agents own architecture, implementation,
tests, debugging, dependency replacement, conflict resolution, integration and
production qualification. Communicate outcomes and evidence; never ask the CEO
to inspect code, perform technical steps, or provide routine engineering review.

This delivery is **Step 1 of the CEO's 100-step vision**. A CEO must be able to
build, test, improve and use Susi through agents and Susi itself. Finish and
measure the native local delivery loop in `docs/step1-acceptance.md` before
declaring Step 1 complete or moving its unfinished obligations into later steps.

The constitution is `.agents/identity.json`, compiled into the runtime. Mandates
62–71 express the CEO's ten requirements; revised Mandates 43–54 and 61 remove
conflicting historical shell/GitHub/human-engineering prerequisites. Existing
scripts, remote workflows and installed binaries can still implement the old
behavior: treat that as migration work, not as authority to disregard the CEO.
These are requirements, **not claims of completed implementation**. Consult
`docs/native-rust-direction.md` and live verification for current status.

| Mandate | Required outcome |
| --- | --- |
| 62 | Agent-only end-to-end delivery; no CEO code inspection or technical repair. |
| 63 | Rust-only Susi runtime, build/install/release tooling and shipped implementation dependencies. Rust facades do not turn C/C++ into Rust. |
| 64 | Native Windows is first-class: MSVC and native OS APIs, no Bash/WSL/MSYS/Python/Node/container prerequisite. |
| 65 | Parallel agents with private source, build and runtime state, disjoint scopes, serialized tested integration and measured speed. |
| 66 | Development, integration and debug releases work locally without GitHub; hosting/publication are optional. |
| 67 | Separate debug and optimized production artifacts; evidence-gated atomic promotion, health checks and verified rollback. |
| 68 | Apache-2.0, MIT, BSD-2-Clause and BSD-3-Clause only for distributed components; correct SPDX interpretation, preserved notices and no audit exclusions hiding shipped violations. |
| 69 | Zero bloat through justified capabilities and measured size, memory, latency and build budgets. |
| 70 | Zero required user-written configuration; discovery, defaults and repair are agents' responsibility. |
| 71 | **Susi: AI that earns its next upgrade.** Promote demonstrated improvement, with durable evidence and rollback. |

OS ABI calls and installed compiler/linker tools are explicit platform boundaries,
not permission to bundle foreign runtimes. Record external protocol peers and
service licenses accurately; connecting to a service does not relicense it or
certify its compliance. Credentials, privacy consent, paid-service authorization,
spend caps and destructive-action authorization remain explicit. Never weaken
safety, truth or license checks to manufacture a passing result.

License qualification must include code copied from the Rust standard library
and source-specific license exceptions inside selected dependencies. A passing
Cargo SPDX check does not override nested notices or generated-table provenance.
Treat installed compiler tools as platform boundaries, but audit standard-library
implementation statically linked into Susi. Preserve original attribution and
bind any custom standard-library replacement to its source, builder, compiler,
selected dependency graph and actual linked artifacts before production approval.

## Native local agent loop

1. Inspect the actual repository, local integration head and existing changes.
   Preserve others' work. Do not work in the primary checkout or on `main`.
2. Reserve a unique identity and disjoint scopes in a private branch/worktree.
   `cargo local-workflow start <name> --scope <path>` is the available native
   reservation path. `cargo local-workflow list` inspects local reservations.
   Bootstrap Cargo itself must have a private output directory; do not inherit
   a shared `CARGO_TARGET_DIR` or a target symlink/junction. Safe immutable caches
   may be shared. Never share writable worktrees, target outputs, runtime homes,
   credentials or ports between workers.
3. Record a task under `.agents/tasks/` with namespaced `T-<AGENT>-<n>`, explicit
   dependencies/scopes and meaningful acceptance. One live claim per worker.
   Separate independent work across isolated agents; do not acquire overlapping
   scopes or edit another agent's files. A missing native coordination feature
   is work to complete, not a reason to fall back to required GitHub claims.
4. Implement and verify locally. `cargo local-build debug` builds the native
   development executable. `cargo local-build production` is a candidate build,
   not proof of a qualified release. Neither installs over production.
   Run tests in per-agent scratch state, never real `~/.susi`, shared
   `~/.susi-dev`, inherited `SUSI_HOME` or real provider keys.
5. `cargo local-workflow commit <name> "<message>"` checks scoped staged,
   unstaged and untracked edits before committing locally. The current native
   launcher records `Task: LOCAL-<name>`; task-based commits use
   `Task: T-<AGENT>-<n>`. These receipts identify ownership, not test success.
   Never disable shared hooks globally. Legacy network hooks are not the local
   correctness gate; use the native command instead of handing work to GitHub.
6. Serialize integration, validate against the current local integration head,
   run acceptance on the **actual combined tree**, and publish it atomically.
   A clean Git merge is insufficient. A missing integration command or failed
   gate remains an open task; do not invent success or bypass checks. Preserve
   both workers' changes and resolve conflicts as agents. Local completion must
   not depend on `origin/main`, a pull request, remote CI, or human code review.
7. Close only after acceptance and integrated-tree verification succeed for the
   complete task. Zero executed tests do not count. Release ownership deliberately;
   `cargo local-workflow release <name>` preserves the branch/worktree and refuses
   dirty state. A partial result or stale receipt never closes a broader mandate.

The native harness must eventually own this whole loop (Mandate 61). Available
commands are building blocks; reservations alone are not enforced filesystem
isolation, and compilation alone is not production qualification. Cross-host
workers need one shared coordinator, not independent local locks.

## Production and self-improvement

Keep the installed production executable and daemon stable while building its
successor. Production qualification requires an identified source tree and
locked graph, strict license and Rust-only audits, format/lint checks, nonempty
required tests, executable smoke checks, artifact digest and reproducible
source/toolchain/target/feature evidence. Then atomically promote, retain the
previous verified artifact, check health, and roll back on failure. Test the
promotion/rollback path, including Windows file locks and interruption.

The CEO grants standing authority to **commit, push and merge** Susi work at
agents' discretion. Agents perform automated review and required testing; the
integrator serializes merges and verifies the actual combined tree. Do not ask
for routine CEO approval of those Git actions. Preserve other agents' work and
never force-push or bypass evidence gates. GitHub publication remains optional
and is not a local-development prerequisite.

This Git authority does not authorize paid services, destructive operations or
unrelated external publication. Production promotion still requires all local
evidence, health and rollback gates above. Never replace production with an unqualified
debug or optimized binary. Generating an embedding is not durable memory;
self-improvement claims require persisted and retrievable outcome evidence.

# Agent Engineering Mandates

## Enforced (compiler — violations fail the build)

- **`unsafe` is forbidden or denied in every crate.**
  - `#![forbid(unsafe_code)]`: susi-abi, susi-adapters-llm, susi-core,
    susi-dsh-cell, susi-error, susi-gawd, susi-gawd-agents, susi-gmcp,
    susi-http-transport, susi-leaf-services, susi-native-client, susi-paths,
    susi-sandbox, susi-sandbox-client, susi-server, susi-universal-cell,
    susi-vendor-agents, susi-vendor-candle, susi-vendor-chrome,
    susi-vendor-cloud, susi-vendor-fastembed, susi-vendor-mcp,
    susi-vendor-mcp-server, susi-vendor-models, susi-vendor-syn,
    susi-vendor-tantivy, susi-vendor-wasmtime, susi-vendor-web, xtask, root
    package.
  - `#![deny(unsafe_code)]` + per-function `#[allow(unsafe_code)]` with a
    `// SAFETY:` justification: susi-agents, susi-config, susi-daemon,
    susi-gawd-a2a, susi-gawd-swarm, susi-gemi, susi-gemi-models, susi-tools.
    Justified sites are FFI only (libc syscalls, candle mmap, env mutation in
    tests).
  - `susi-windows-runtime` also denies unsafe code by default. Its qualified
    Windows process-entry, TLS and exception ABI boundary permits only narrow
    per-function or per-FFI-metadata-item allowances with `// SAFETY:` evidence.
    Never allow unsafe code for the whole module or copy foreign CRT startup
    implementations. Preserve lifecycle tests and artifact-bound link-map checks.
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
  (`susi-vendor-candle`, `susi-vendor-wasmtime`, `susi-vendor-mcp*`, …) or a
  pure re-export facade (`susi-vendor-serde`, `susi-vendor-tokio`, …).
  Consumers write `serde = { package = "susi-vendor-serde", path = "../susi-vendor-serde" }`
  so `use serde::…` is unchanged. A new external crate means a new facade —
  `susi_crates_declare_zero_external_dependencies` (architecture_tests)
  enforces it.
- **Vendor *code* lives in vendor crates, not only vendor deps.** Any code
  that speaks a third-party agent, model-provider, or web API must live in a
  `susi-vendor-*` crate: `susi-vendor-agents` (external agent executors +
  framework adapters), `susi-vendor-models` (cloud/frontier/OpenRouter/
  open-weight/HF discovery), `susi-vendor-web` (admitted search-tool evidence; no built-in weather or search provider),
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

## Local verification and evidence

Use the pinned toolchain and committed lockfile. Scope checks to affected crates
and their dependents during development, and run the complete required suite
before qualifying production. Do not run workspace-wide `--all-features` when
CUDA/MKL/Metal are mutually exclusive. A passing compile is not a passing test.

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets --offline --locked -- -D warnings
cargo test --workspace --offline --locked
cargo deny --offline --locked check licenses
```

Offline Cargo resolution does not sandbox build-script network calls. Provision
sources/toolchains explicitly and verify build scripts as part of the offline
contract. A warm cache is not evidence of clean-machine independence.

Evidence and task IDs are namespaced per agent. Prefer one file per record.
Union conflicting evidence entries; never delete or renumber another agent's
history. Keep historical mandate IDs stable when revising requirements. JSON
and schema validation must use native tooling in the final product path.

Preserve coverage, unsafe-exposure and architecture ratchets. New production
modules must have actual production wiring; discarded probes or type-only uses
do not count. No stubs, fake-success paths or lowered baselines to pass a gate.

`CLAUDE.md`, `GEMINI.md` and `.github/copilot-instructions.md` point here without
copying a divergent workflow. `susi_core::self_build::BRIEF` carries the same
contract into delegated tasks. Update them and governance tests together when
requirements change. Legacy scripts and GitHub adapters may remain during
migration, but must not become prerequisites or contradict these mandates.
