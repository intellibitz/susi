# Forking and contributing to Susi (`intellibitz/susi`)

Susi core is open source under the [Apache License, Version 2.0](LICENSE). It is
**published one way** (CEO directive 2026-11-16, Mandate 72 in
[`AGENTS.md`](AGENTS.md)).

## 1. How this repository is published

- **One publisher.** IntelliBitz's CEO alone holds push authority over this
  repository. It is exercised by the CEO or by the agents the CEO authorizes.
- **Nothing is accepted from outside.** No pull request, patch or commit from
  an outside party is reviewed, accepted or merged, and no outside party is
  given write access. Please do not open one: it will not be merged.
- **You are free to fork.** Fork the repository and work in your fork. You need
  no permission and no agreement beyond the license.

This is a statement about how the repository is run. It takes nothing away from
the rights the license gives you.

## 2. What the license lets you do

The [Apache License 2.0](LICENSE) lets anyone copy, use, change and
redistribute this code, in source or binary form, for any purpose including a
commercial one. When you redistribute it, the license asks you to:

1. give recipients a copy of the license;
2. mark the files you changed as changed;
3. keep the copyright, patent, trademark and attribution notices of the source
   you took, and the content of any `NOTICE` file it carries.

The license grants code rights, not rights to names. "Susi" and "Susi
Certified" are IntelliBitz marks (Apache-2.0 section 6): a fork must not present
itself as the official Susi or as certified by IntelliBitz. The code comes as
is, without warranty.

## 3. Build on Susi without forking

Most products need no fork. Core is a lean, vendor-neutral socket
([`AGENTS.md`](AGENTS.md), "Open-core boundary"), and what you build attaches to
it from the outside:

- **The host contract (`9090–9094`).** MCP on `9090` and `9093`, the
  OpenAI-compatible and Anthropic Messages router on `9091`, A2A and ACP on
  `9094`, cluster discovery on `9092`. See [docs/integrating-with-susi.md](docs/integrating-with-susi.md)
  and check your integration with the black-box conformance checker
  ([docs/host-contract-conformance.md](docs/host-contract-conformance.md)).
- **Extension packs and Wasm reflexes.** MCP server catalogs, execution-agent
  adapters and deterministic `wasm32-wasip1` reflexes are external plugs. Follow
  the [extension pack and Wasm reflex starter guide](docs/extension-pack.md) and
  validate your pack with:
  ```bash
  susi extensions validate ./your-pack-dir
  ```

A plug you write is yours. It lives in your repository under the license you
choose, and core admits it over the socket.

## 4. Working in your fork

A fork may change anything. If you want your fork to stay close to this
repository, these are the invariants its own tests enforce
(`tests/architecture_tests.rs`, `tests/core_docs_promise_no_ui_or_plugins.rs`):

1. **Pure CLI plus standards socket.** Core ships no UI (no HTML, CSS, JS, GUI
   or TUI crates) and bundles no third-party plugin catalogs.
2. **Zero `unsafe` and zero production panics.** Workspace crates forbid or
   deny `unsafe_code`. Production code returns typed `EaiResult<T>`
   (`crates/susi-error`); `unwrap()`, `expect()`, `panic!()`, `todo!()` and
   `unimplemented!()` are denied by Clippy unless an inline `#[allow(...)]`
   carries a written justification.
3. **Facade dependency rule.** `susi-*` crates declare zero direct third-party
   dependencies; each external crate is wrapped once in a `susi-vendor-*`
   facade crate.
4. **Permissive licenses only.** Every distributed dependency is `Apache-2.0`,
   `MIT`, `BSD-2-Clause` or `BSD-3-Clause` (`cargo deny check licenses`).

### Setup

```bash
git clone https://github.com/<you>/susi.git
cd susi
scripts/setup-dev.sh
```

`scripts/setup-dev.sh` configures the repository hooks and merge drivers.
Pre-commit checks staged Rust formatting; pre-push enforces transport safety
for this repository's own storage rules, which a fork may replace.

### Checks

```bash
# 1. Formatting
cargo fmt --all --check

# 2. Lints
cargo clippy --workspace --all-targets --locked -- -D warnings

# 3. Architecture and documentation contract tests
cargo test --test core_docs_promise_no_ui_or_plugins --locked
cargo test --test architecture_tests --locked

# 4. License compliance
cargo deny --locked check licenses
```

### A live, isolated host

Never test against your own `~/.susi` state. Start an isolated instance with
`SUSI_HOME` and `SUSI_PORT_OFFSET`:

```bash
SUSI_HOME=/tmp/susi-dev SUSI_PORT_OFFSET=700 cargo run -- start
cargo run -p susi-conformance -- --port-offset 700 --token-file /tmp/susi-dev/api_token
SUSI_HOME=/tmp/susi-dev SUSI_PORT_OFFSET=700 cargo run -- stop
```

## 5. If your fork has something Susi wants

Nothing flows back by request. Susi takes existing Apache-2.0 work by its own
harvest: a pinned commit, per-file license verification, preserved notices and
recorded attribution ([docs/harvest.md](docs/harvest.md)). A fork of Susi is an
Apache-2.0 project like any other, so that is the only way its code can return,
and it is Susi's decision.

## 6. Reporting a security problem

Follow [SECURITY.md](SECURITY.md). A report is information, not a contribution:
the fix is written and published by the CEO's agents.

## 7. The CEO's agents

Agents the CEO has authorized follow [AGENTS.md](AGENTS.md): agent roots, the
Susi Board, task claims and the vision cycle. They use `susi-commercial
workflow start`, `sync`, `commit` and `ship` from an isolated worktree, reconcile
with `origin/main` at session start, on resume and before committing, and label
commits with `Task:` and `Feature:` trailers.

`main` is an unqualified raw-source stream with no mandatory publication test
gate. Susi distills selected work into release candidates and qualifies the
exact combined tree before publishing immutable release snapshots. See the
[CEO agent mandate](docs/ceo-agent-mandate.md) and
[Mandate 75](docs/ceo-mandate-75.md).

## 8. License

Everything in `intellibitz/susi` is licensed under the
[Apache License, Version 2.0](LICENSE).
