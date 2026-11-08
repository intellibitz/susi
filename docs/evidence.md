# Evidence: verified claims, platforms and qualification

This page holds the record the front door used to carry: the claims that hold
in source, the platform and release scope, the agent-owned delivery path, and
every admitted gap. The sections below were moved verbatim from `README.md`
on 2026-11-08 (T-README-1) and are kept as written, with their original dates
and numbers; nothing was softened or dropped. The installed-journey record
itself has always lived in [`docs/step1-acceptance.md`](step1-acceptance.md)
and is linked, not copied, from the last two sections.

Reading rule: a row describes the tree on the day it was written. Where a row
names a crate that has since left core under the lean-core program, the
[`docs/lean-core.md`](lean-core.md) wave log is the record of when and why.
On 2026-11-08 these named crates are no longer in `crates/`: `susi-vendor-mcp`
(replaced by a std JSON-RPC client in `susi-tools`, iteration 362),
`susi-vendor-candle`, `susi-vendor-cloud`, `susi-vendor-chrome`,
`susi-vendor-tantivy`, `susi-vendor-fastembed`, `susi-vendor-models`,
`susi-vendor-agents`, `susi-vendor-web`, `susi-sandbox`, `susi-agents`,
`susi-universal-cell`, `susi-vendor-jsonschema` and `susi-vendor-sysinfo`.
The one Wasm crate, `susi-vendor-wasmtime`, is a compatibility name: it runs
Wasmi, the pure-Rust interpreter, and links no Wasmtime (`Cargo.lock` holds
`wasmi` and no `wasmtime`; a test in that crate pins it).

## Verified product claims

These are the claims that hold in source (not marketing unbounded “any protocol”):

| Claim | Holds as |
|-------|----------|
| Tier S crown | `susi crown verify` — Truth/Evidence/Swarm/Blackboard/Glass/Audit/Auto/Governance/Pluggable/Sandbox/Host/Reflexes/Provision/Concurrency |
| Agent-of-agents / OS layer for AI | Multi-agent swarm + host daemon/ports — layer *for* agents, not a host OS (Mandate 45) |
| Admission protocol: agents | executors and frameworks admitted from an installed pack or `~/.susi/execution-agents/` adapter file; core bundles no agent catalog; UNAVAILABLE until driver/config present |
| Admission protocol: engines | inference endpoints the user configures (+ discovered local ports); core starts none at boot |
| Admission protocol: models | compiled coding / frontier / open-weight / OpenRouter catalog views + live `/models`; local GGUF / Ollama |
| Admission protocol: MCP | any server admitted explicitly (`susi mcp-add`, or `susi mcp enable` from an installed pack); core bundles no MCP server catalog |
| Bloat audit | `susi bloat-audit` runs the AST auditor directly (no model narration) and scans every non-test Rust source in the workspace; last measured: 967 files, ~275k lines, 28 `unwrap`, 63 `expect`, 32 `unsafe` blocks — each panic-path `#[allow]` carries a written justification (architecture test) |
| Substrate report | `susi substrate-report` reports the compiled constitution, topology and selected model; the GMCP `sovereign_dashboard` report tool counts audited activity by its real event type (no invented self-heal/distillation counters) |
| Version sync | `susi admin sync` replaces Cargo.toml, the README badge, the governance ledgers, and the binary trust anchor atomically |
| Audit commands | `susi audit` / `susi admin audit` run the static compliance audit; `admin verify` checks version alignment; `admin lint` / `admin audit-deps` run `cargo clippy` / `cargo audit` directly — no model narration |
| Model ladder | `susi verify-download-agent` reports network status and each step of the hardware-selected model ladder (no fixed 32B/72B promise) |
| Self-validation | `self_validate` reports the hardware profile and tensor device and checks the compiled genome; a failure is an error, never a successful receipt |
| Install | `susi install` runs the substrate initializer directly and reports its real outcome |
| Model override | `susi select-model <id>` writes the override directly and rejects empty or multi-line ids |
| Workspace changes | applied through transactions (`susi tx`, `apply_patch_cycle`); the unused staged-bundle commands (`accept` / `undo`) were removed |
| Cleanup | `susi os clean` (alias `susi os-clean`) removes only SUSI-owned residue: pre-cap rotated metrics, a stale flat audit log, and oversized stray binaries |
| Configuration | every key in the bundled `config.default.json` is read by code (orphan keys such as `beacon_interval_secs` were removed) |
| Intent memory | `susi` natural-intent ingestion types entries with the config-driven `intent_classify` lists (whole words), dedupes only exact repeats, redacts credentials, and writes the ledger atomically |
| Compliance | `susi admin` audits real source/governance state; no self-certifying capability checklist ships (a hard-coded 'AGI compliance certificate' module was removed) |
| Daemon feature surface | live swarm-host components are wired from `composition`. The daemon loop constructs host OS planes and drives them: per-peer HMAC gossip UDP 9095 (HMAC-sealed `gossip_caps.json`; sealing/ingress fail closed when no MAC key exists), egress-gated STUN with honest DNS/timeout errors (skipped while `mac_policy` blocks external egress unless `SUSI_TURN_*` is configured), TURN Allocate with RFC 5389 MESSAGE-INTEGRITY (`SUSI_TURN_USER`/`SUSI_TURN_PASS`, or `SUSI_TURN_RELAY`), no cloud CLI run at boot or on the tick (core starts nothing on its own; `susi os provision probe` version-probes on request), and `os_planes.json` for `susi os`. `susi os provision probe\|list\|apply` is the explicit list/apply surface. Deleted cell-OS sketches are not resurrected; `live_planes` maps those needs onto policy/task/sandbox/checkpoint. Unwired daemon-module ratchet is 0. |
| Shared foundation | shared code is reached through Cargo edges that point down the leaf order (`paths → error → config → core/sandbox → services → daemon`, enforced by an architecture test). `susi-paths`, `susi-error`, `susi-config`, `susi-sandbox-client`, `susi-core`, `susi-abi`, `susi-http-transport` (the one crate that declares `ureq`, caps response bodies, and exposes HTTP status; callers use `http_call` and never name `ureq::Agent`), `susi-vendor-candle` (Candle + tokenizers) and `susi-adapters-llm` (wire shapes + `post_json` over transport) are single crates — no crate except `susi-http-transport` and the `susi-vendor-*`/`susi-adapters-*` crates declares `reqwest` or `ureq` (an allow-list test, so new crates are covered automatically); `susi-gemi-models` declares neither `reqwest` nor `tokenizers`. Remaining cross-crate `#[path]` mounts are counted by a ratchet that only decreases (159 → 0) |
| Vendor crates | every third-party SDK lives in its own `susi-vendor-*` crate that exposes SUSI-shaped calls and no vendor types: `susi-vendor-candle` (Candle/tokenizers), `susi-vendor-wasmtime` (Wasmi/bounded WASI + metered cells), `susi-vendor-mcp` (rmcp MCP client + its reqwest transport), `susi-vendor-mcp-server` (rmcp MCP server + `#[tool]`; GMCP never declares `rmcp`), `susi-vendor-cloud` (kubectl/docker/aws/gcloud/az CLIs — no cloud SDKs, ≤ 1 file; `susi-vendor-agents` ≤ 9 files; `susi-vendor-models` ≤ 37 files), `susi-vendor-tantivy` (full-text `DocIndex`), `susi-vendor-fastembed` (one process-wide embedding model; cached load failures feed provider health), `susi-vendor-chrome` (headless Chrome page capture), `susi-vendor-syn` (Rust AST: validity, bloat metrics, outline); the A2A SDK (`ra2a`) is confined to its own plane crate, `susi-gawd-a2a`. Beyond SDKs, every remaining third-party crate is owned by a pure re-export facade (`susi-vendor-serde`, `susi-vendor-tokio`, `susi-vendor-hyper`, … — 48 crates), so susi-* crates declare **zero** third-party dependencies in any dep section while `use serde::…`-style paths compile unchanged (`susi_crates_declare_zero_external_dependencies` ratchet) |
| One loopback client | the paths, config, sandbox, native and error-event clients share `susi_paths::loopback` (std-only bounded request / status / body / bearer / instance-offset port; response cap 16 MiB) instead of five hand-rolled HTTP/1.0 clients; every leaf-service client now honours the instance port offset, so a dev instance never reaches the release instance's services |
| One error model | every library crate returns `susi_error::EaiResult`; `anyhow` remains only in the root CLI application. `ResultExt::context` keeps the underlying error's kind (I/O stays Io, JSON stays Config) and rewrites the message in place, so a contextualized failure is one metrics event, not two |
| Dependency hygiene | every declared dependency is used by its crate (63 dead declarations removed in one pass: `crossbeam` alone was declared by 11 crates and used by 1); every external crate is declared by exactly one `susi-vendor-*` crate — susi-* manifests contain only workspace members; `susi-paths` resolves per-OS user dirs std-only (no `directories`); std `LazyLock` replaces `once_cell`; the pulse queue runs on the workspace's one channel crate (`flume`) |
| Leaf service shells | the five leaf services (paths, error, config, sandbox, native) are served by one crate, `susi-leaf-services` — the only place they meet axum; the foundation libraries carry no HTTP framework or async runtime; standalone bins enable only their service feature so unrelated Wasmi/Bollard backends stay out, while the root enables `all-services` for `service-run`; all take their port from the one `LEAF_SERVICES` table |
| Native service boundary | Wasmi is linked only by the compatibility-named `susi-vendor-wasmtime` (WASI reflexes for the `susi-native` service + the metered in-process cell runtime the daemon uses; the daemon no longer carries its own wasmtime/wasm-encoder dependencies); feature planes share one compiled, typed `susi-native-client` instead of source-mounting a misleading `vendor_template` copy |
| Repository hygiene | the root carries no scratch artifacts (a stray 4.5 MB `test_verify` binary and five unreferenced experiment files were removed); duplicate root `serde_json` and GMCP `tokio` dev-dependency declarations were removed; leftover mount-era crypto and HTTP deps were dropped from twelve crates after the contract remounts; GPU features exist only where Candle reads them (`susi-vendor-candle`, forwarded by `susi-gemi` / `susi-gemi-models`) |
| Automation | `susi automate <intent>` — evidence-gated swarm mission (Mandate 46; not unbounded autopilot). A failed plan step reports that earlier steps' workspace changes were not rolled back |
| Harness orchestration | `susi harness` — durable self-build orchestration: supervisor, scheduler, pricing catalogue accounting, budget admission, direct provider routing, and operator JSON/SSE API ([`docs/harness.md`](harness.md), [`docs/harness-os.md`](harness-os.md)) |

---

## Platforms

Release and CI scope (CEO decision 2026-11-03): the free public core is built,
tested in CI, and published for vanilla x86_64 Windows and x86_64 Linux only.
Other platforms stay portable in the source so the Susi commercial suite can
build and qualify its own "Susi Certified" binaries in the commercial pipeline;
core runs no CI legs and publishes no assets for them.

| Platform | Core release asset | Installers | Source support |
|----------|--------------------|------------|----------------|
| Windows x86_64 (MSVC) | `susi-windows-x86_64.exe` | `install.ps1` | qualified ([Step 1 acceptance](step1-acceptance.md)) |
| Linux x86_64 | `susi-linux-x86_64` | `install.sh` | yes |
| Windows ARM64 (MSVC) | none | `install.ps1` refuses | portable runtime/link-layer source and dependency-graph checks (`tests/zc_matrix.rs`); no core CI leg; not qualified by core |
| Linux ARM64 | none | `install.sh` refuses | portable source and dependency-graph checks (`tests/zc_matrix.rs`); no core CI leg; qualified in `susi-commercial-apps` |
| macOS | none | `install.sh` refuses | portable source and dependency-graph checks (`tests/zc_matrix.rs`); no core CI leg; qualified in `susi-commercial-apps` |

## Developing susi: agent-owned native delivery (Mandate 48)

The CEO never inspects code. Agents own development, verification, integration
and production qualification. The installed production Susi stays stable while
agents build candidates in private worktrees, build directories and runtime
homes. Do not test against production or share `.susi-dev` between workers.

Installed Susi exposes `susi local build debug`; an optimized candidate uses
`susi local build production`. For the initial build before Susi is installed,
agents use the Rust [native bootstrap](native-bootstrap.md).
A candidate is not an approved
release. Production qualification must bind the exact source/dependency/toolchain
identity to passing tests and policy audits, executable smoke tests and artifact
digests, followed by atomic promotion, health verification and tested rollback.
The strict dependency license gate passes with the replacement implementations;
future dependency changes must pass the same unchanged policy.

## Building susi: the workflow (Mandates 49–72)

Read [`AGENTS.md`](../AGENTS.md), the canonical instructions for every agent.
The native local workflow has no GitHub account, Actions, pull-request or remote
claim prerequisite. Use separate branches/worktrees and disjoint task scopes:

```text
susi local workflow start worker-a --scope crates/susi-paths
susi local workflow list
```

Inside that worker's worktree, implement and verify its task, then use
`susi local workflow commit worker-a "message"` for a scope-checked local commit.
The command checks staged, unstaged and untracked files and does not change
shared hook settings. It records ownership, not test success. Task records under
`.agents/tasks/` use namespaced IDs and meaningful acceptance; task-based commits
carry `Task: T-<AGENT>-<n>` and the native launcher records `Task: LOCAL-<name>`.
`.agents/lanes/` documents history and never substitutes for a reservation.

Integration must serialize changes and verify the actual combined tree against
the current local head. Close a task only after its complete acceptance passes
there. A clean merge, a compile, zero selected tests or a remote CI result is
not enough. `susi local workflow integrate worker-a` performs those local gates.
`susi local workflow ship worker-a` is the optional GitHub publication: one local
gate for the batch, a direct fast-forward push to `refs/heads/main` on `origin`,
and four local commit statuses on the published commit.
GitHub Actions is disabled and its workflow files were removed (CEO decision
2026-11-06); it is not a merge gate.
`susi local release qualify` independently qualifies the integrated source.

`CLAUDE.md`, `GEMINI.md` and `.github/copilot-instructions.md` are identical
pointers to the canonical contract. Automatic shell-based SessionStart hooks
have been removed. Optional remote publishing adapters remain historical
compatibility tooling and cannot govern local engineering. See
[`docs/native-rust-direction.md`](native-rust-direction.md) for verified
implementation status, and [`docs/cloud-sessions.md`](cloud-sessions.md)
for isolated cloud workers.

## Native Windows

The agent-owned Windows path uses native Rust executables, MSVC and OS APIs.
There is no Bash, WSL, MSYS, Python or Node prerequisite. Release receipts retain
the exact source, lockfile, compiler, target, artifact and measured gate results.

Interactive installation shows usage once; give Susi a goal right away with
`susi "<what you want done>"` (or pipe it on stdin), without restarting the
terminal or editing configuration. The core CLI takes one-shot goals; the
interactive conversation lives in the Susi desktop app (susi-commercial-apps).
Discovered credentials and cached observations are checked when used; they are
not a claim that a provider is responding. Optional agent and framework checks
do not delay a goal.

For unattended installation, use `-NoLaunch` when running the downloaded script
or set `NONINTERACTIVE=1`. An absolute `SUSI_HOME` installs an isolated instance
without changing persistent User PATH or registering a service. The installer
keeps one executable and removes an old duplicate only after replacement succeeds.
`susi setup` shows readiness without changing settings; `susi setup plan --json`
retains machine-readable discovery and `susi setup apply --json` applies defaults
without replacing existing configuration.

---

## Installed journey and production qualification

The clean-machine installed journey, the production releases and their
promotion and rollback evidence are recorded step by step in
[`docs/step1-acceptance.md`](step1-acceptance.md):

- [Clean Windows installed journey passed (2026-11-02)](step1-acceptance.md#clean-windows-installed-journey-passed-2026-11-02)
- [Combined verification and remaining release qualification (2026-11-02)](step1-acceptance.md#combined-verification-and-remaining-release-qualification-2026-11-02)
- [Production release C: clean 8 GiB journey, promotion and rollback (2026-11-02)](step1-acceptance.md#production-release-c-clean-8-gib-journey-promotion-and-rollback-2026-11-02)
- [Production release D: slow downloads and tool-free recovery answers (2026-11-02)](step1-acceptance.md#production-release-d-slow-downloads-and-tool-free-recovery-answers-2026-11-02)
- [Change to live on the dev channel under 60 seconds (2026-11-06)](step1-acceptance.md#change-to-live-on-the-dev-channel-under-60-seconds-2026-11-06)
- [User-response budgets (2026-11-06, T-RTBUDGET-1)](step1-acceptance.md#user-response-budgets-2026-11-06-t-rtbudget-1)

Verified implementation status of the native Rust delivery path, including
the license and standard-library provenance findings, is in
[`docs/native-rust-direction.md`](native-rust-direction.md). Host-contract
conformance against a running host is
[`docs/host-contract-conformance.md`](host-contract-conformance.md). The
compile-budget measurements are the table in
[`docs/lean-core.md`](lean-core.md#compile-budget-60-seconds-ceo-mandate-2026-11-07-2250).

## Admitted gaps on record (2026-11-08)

Each line names what is not delivered and where the record is. These are the
project's own statements; a gap leaves this list only when its record says so.

- Known gap: Step 1 remains incomplete until the final combined source publishes
  an acceptance receipt, the optimized product qualifies, that exact production
  artifact passes the clean Windows installed journey, and genuine A/B
  promotion, runtime health and rollback are demonstrated
  (`docs/step1-acceptance.md`, "Combined verification and remaining release
  qualification").
- Known gap: the user-response budgets are declared and met on a quiet host for
  the flush-fix build only; the quiet-host confirmation of the both-fixes build
  has not been taken, and production qualification does not yet run
  `startup-gate` or the 2000 ms host-contract conformance budget, so a release
  that misses them is not refused (`docs/step1-acceptance.md`, "User-response
  budgets").
- Known gap: about 0.2-0.4 s of a first goal is still the one-file-per-tool
  rendezvous publication (same section).
- Known gap: the in-process GGUF model runtime left core in ungated iterations
  341-347 (2026-11-07) with no destination on the commercial origin;
  `tests/zc_matrix.rs` keeps its two runtime tests ignored by CEO decision
  (2026-11-08 02:00, "land with ignores"). A default build (`local-engine`
  feature off) serves local in-process inference only through a plug wired on
  the plane bus (the commercial `susi-plug-host`); otherwise local inference
  goes to the HTTP engines you run (Ollama, llama.cpp, vLLM, ...) or to cloud
  providers.
- Known gap: embeddings (`gpu_fabric.embed.*`) and browser page capture
  (`desktop.browser.capture`) are plane-bus plug ports in core; without the
  commercial plug host they report a typed capability gap
  (`docs/lean-core.md`, T-PLUGPORTS-1).
- Known gap: the compile budget is 60 s for a cold `cargo build -p susi`; the
  last idle measurement on landed main `240ee4dfb` is 81.7 s wall and 822.7
  rustc CPU-seconds (from 196 s under load and 165 s idle at the start of the
  program). Lean-core rungs 2-4 (std-only socket, last dependencies, offline
  clean clone) are open (`docs/lean-core.md`).
- Known gap: the clean-machine installed journey on record is the Windows
  x86_64 one; Linux x86_64 is released from the same source and its source
  support is recorded, but no equivalent core journey record exists for it
  (Platforms table above, `docs/step1-acceptance.md`).
- Known gap: the governance vetoes (safety, security, privacy) catch
  known-dangerous shapes by pattern; they are not a sandbox boundary. The
  sandbox boundary is the Wasmi host for Wasm modules and Docker for
  `sandbox_exec` where present. A threat model is a separate document, not yet
  written.
