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

Before opening a Pull Request, please keep these repository invariants in mind (enforced by `tests/architecture_tests.rs` and `tests/core_docs_promise_no_ui_or_plugins.rs`):

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

`scripts/setup-dev.sh` configures `.githooks` (`rustfmt` on pre-commit, scoped `clippy` on pre-push) and the union merge driver for `.agents/evidence.json`.

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

### External Human Contributors (Fork & Pull Request)
1. Fork `intellibitz/susi` and create a feature branch (`git checkout -b feat/my-improvement`).
2. Make your changes and add unit or integration tests covering both the happy path and relevant failure mode.
3. Verify formatting, Clippy, and tests locally.
4. Open a Pull Request against `main` using the PR template. You do **not** need to hand-edit `.agents/evidence.json` or `.agents/tasks/` in external fork PRs—maintainers and repository agents reconcile task ledgers during integration.

### Autonomous Agents (`cargo local-workflow`)
Autonomous agents operating inside a local multi-worktree environment must follow [`AGENTS.md`](AGENTS.md):
```bash
cargo local-workflow start worker-a --scope crates/susi-paths
# Implement and verify in the isolated worktree
cargo local-workflow commit worker-a "feat(paths): description"
cargo local-workflow integrate worker-a
```

---

## 5. License

By contributing to `intellibitz/susi`, you agree that your contributions will be licensed under the [Apache License, Version 2.0](LICENSE).
