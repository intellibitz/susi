# Contributing to SUSI (`intellibitz/susi`)

Thank you for your interest in contributing to **SUSI** — the Apache-2.0 open-source AI OS socket for local and cloud AI ecosystems!

SUSI welcomes contributions from **external open-source engineers**, **protocol/MCP/A2A implementers**, **extension-pack & Wasm-reflex authors**, and **autonomous AI agents**.

---

## 1. Where You Can Contribute

### A. Protocol Conformance & Host Socket (`9090–9094`)
- **MCP (`9090` / `9093`)**: Protocol revisions (`2026-07-28` stateless and `2025-06-18` session), pagination, SSE streaming, and `susi-conformance` black-box checks (`crates/susi-gmcp`, `crates/susi-conformance`).
- **OpenAI & Anthropic Inference Router (`9091`)**: Request validation, streaming SSE parity, placement headers (`X-Susi-Placement`, `X-Susi-Placement-Id`), and local/cloud failover (`crates/susi-gemi`).
- **A2A Discovery & JSON-RPC (`9092` / `9094`)**: Agent card discovery (`/.well-known/agent-card.json`) and peer federation (`crates/susi-gawd-a2a`, `crates/susi-gawd-swarm`).

### B. Community Extension Packs & Wasm Reflexes (External Plugs)
- Core is intentionally a lean, vendor-neutral socket (`AGENTS.md` → *Open-core boundary*). Third-party integrations—MCP server catalogs, execution-agent adapters (`openhands`, `gemini-acp`, `a2a`), and deterministic `wasm32-wasip1` reflexes (`reflex_<slug>`)—are built as external plugs!
- Follow the [5-Minute Extension Pack & Wasm Reflex Starter Guide](docs/extension-pack.md) and test your pack with:
  ```bash
  susi extensions validate ./your-pack-dir
  ```

### C. Core Rust Quality, Performance, and Portability
- Hardening error handling (`EaiResult<T>`), reducing build times, improving memory/latency budgets, and maintaining OS portability across Windows (`Schannel`), Linux (`OpenSSL`), and macOS (`Security.framework`).

---

## 2. Open-Core & Architecture Boundaries

Before contributing, please keep these repository invariants in mind (enforced by `tests/architecture_tests.rs` and `tests/core_docs_promise_no_ui_or_plugins.rs`):

1. **Pure CLI + Standards Socket**: Core ships no UI (no HTML/CSS/JS/GUI/TUI crates) and bundles no third-party plugin catalogs. Visual views and curated commercial packs live in `Susi Commercial Apps`.
2. **Zero `unsafe` & Zero Production Panics**:
   - Workspace crates forbid or deny `unsafe_code`.
   - Production code returns typed `EaiResult<T>` (`crates/susi-error`); `unwrap()`, `expect()`, `panic!()`, `todo!()`, and `unimplemented!()` are denied by Clippy unless accompanied by an inline `#[allow(...)]` + written justification comment.
3. **Facade Dependency Rule**: `susi-*` crates declare zero direct third-party dependencies in `Cargo.toml`; external crates are wrapped once in a `susi-vendor-*` facade crate.
4. **Permissive Licenses Only**: All distributed dependencies must use `Apache-2.0`, `MIT`, `BSD-2-Clause`, or `BSD-3-Clause` (`cargo deny check licenses`).

---

## 3. Developer Setup & Local Verification

### Initial Setup
```bash
git clone https://github.com/intellibitz/susi.git
cd susi
scripts/setup-dev.sh
```

`scripts/setup-dev.sh` configures the repository hooks and merge drivers.
Pre-commit checks staged Rust formatting; pre-push enforces transport safety.
Clippy and tests are development checks and release qualification, not mandatory
`main` publication gates. Native workflow commands are available through
`susi-commercial workflow`; the shell setup helper is optional.

### Running the Verification Suite
Run these checks locally before submitting your changes:

```bash
# 1. Formatting check
cargo fmt --all --check

# 2. Scoped or workspace Clippy lint gate
cargo clippy --workspace --all-targets --locked -- -D warnings

# 3. Architecture & documentation contract tests
cargo test --test core_docs_promise_no_ui_or_plugins --locked
cargo test --test architecture_tests --locked

# 4. License compliance check
cargo deny --locked check licenses
```

### Testing Against a Live Isolated Host Instance
Never test against your primary `~/.susi` state; spin up an isolated instance with `SUSI_HOME` and `SUSI_PORT_OFFSET`:

```bash
SUSI_HOME=/tmp/susi-dev SUSI_PORT_OFFSET=700 cargo run -- start
cargo run -p susi-conformance -- --port-offset 700 --token-file /tmp/susi-dev/api_token
SUSI_HOME=/tmp/susi-dev SUSI_PORT_OFFSET=700 cargo run -- stop
```

---

## 4. Submitting Changes

### Portable Git contributions

1. Develop in an isolated clone or worktree and preserve the core boundaries above.
2. Run checks appropriate to the change and include results and known limitations.
3. Share an identified commit or Git ref with an authorized maintainer or agent
   through the project's coordination channel. No hosted pull request is required;
   repository agents must not open, merge or close pull requests.
4. The integrating agent records a task and feature, reconciles `origin/main`,
   and publishes a normal fast-forward update. Only `refs/heads/main` is a remote
   branch; agent work uses `refs/agents/<agent>`.

### Autonomous agents

Follow [AGENTS.md](AGENTS.md), including agent roots, the Susi Board, task claims
and the vision cycle. Use `susi-commercial workflow start`, `sync`, `commit`
and `ship` from your isolated worktree; consult each command's `--help` for
arguments. Fetch and merge `origin/main` at session start, resume and before
committing. Label commits with `Task:` and `Feature:` trailers.

`main` is an unqualified raw-source stream with no mandatory publication test
gate. Susi distills selected work into release candidates and qualifies the
exact combined tree before publishing immutable release snapshots. See the
[CEO agent mandate](docs/ceo-agent-mandate.md) and
[Mandate 75](docs/ceo-mandate-75.md).

---

## 5. License

By contributing to `intellibitz/susi`, you agree that your contributions will be licensed under the [Apache License, Version 2.0](LICENSE).
