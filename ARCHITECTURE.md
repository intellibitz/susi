# SUSI architecture

This document is the **enforceable** crate-boundary contract for the SUSI
substrate. It maps a ports-and-adapters modularization checklist onto SUSI’s
identity — GEMI / GAWD / GMCP, host-contract ports, `CapabilityRegistry`,
extension packs, evidence-gated swarm — **without** renaming the workspace into
a foreign `apps/core/modules/adapters` tree.

Constitutional source of truth for product claims remains
[`.agents/identity.json`](.agents/identity.json). When this file and identity
disagree on host ports or pillar names, **source wins** and both are updated in
the same change.

## Dependency direction (inward)

```text
CLI (susi) / daemon / HTTP servers
        ↓
composition roots (wire hooks, packs, discovery, bind)
        ↓
feature planes: GAWD · GEMI · GMCP · agents · tools
        ↕  (in-process only: `susi_core::plane_bus` — no Cargo edges between planes)
        ↓
susi-core (Evidence, Truth, Provider, Tool, CapabilityRegistry, plane_bus)
        ↓
susi-error · susi-paths
        ↑
infrastructure adapters: sandbox · native (Wasmer) · Docker / HTTP / MCP clients
```

**Rule:** domain and contract code must not import frameworks, databases,
networking clients, UI, OS daemon APIs, Candle, Wasmer, Bollard, or `rmcp`.

Concrete implementations are assembled only at composition roots.

## Local and cloud operating planes

SUSI presents one control plane across two execution ecosystems; these are
placement targets, not separate products:

| Plane | Managed resources | Shared contracts |
|-------|-------------------|------------------|
| **Local** | Candle/GGUF and Ollama models, CPU/GPU residency and eviction, local MCP/CLI agents, Wasm reflexes, Docker sandbox, workspace memory | `CapabilityRegistry`, MAC/privacy policy, evidence receipts, HMAC audit, budgets, placement decision |
| **Cloud** | Configured OpenAI-compatible/Anthropic/Gemini/OpenRouter providers, remote MCP, A2A peers, managed external agents | the same registry, policy, evidence, audit, budget, and placement contracts; credentials remain explicit operator configuration |

`susi os route` and `GET /runtime/placement` execute the same placement policy
used by completion requests. Each executed plan carries a
`placement-<pid>-<unix>-<seq>` id in the response body/SSE/header and records
`INFERENCE_PLACEMENT` in the signed audit chain. Authenticated callers resolve
the correlation through `GET /runtime/placement/decisions/{id}`; the result
includes the audit timestamp and entry hash.

Append-only durability is also a cross-plane contract. Audit writers hold a
cross-process `FileLock`, emit each complete JSONL record with one write, sync
the log before publishing a length-bound atomic tip checkpoint, and rebuild a
stale checkpoint only from a verified chain. Verification never turns an I/O
or UTF-8 failure into an empty valid ledger. Receipt-archive rotation is
cross-process serialized and readers traverse every retained generation.
Semantic-index watermarks advance only after durable vector/index writes and
never consume an unterminated source tail.

## Crate boundaries

| Crate | Responsibility | Public API (shape) | May import | Must not import | Replaceable at runtime | Owns state | I/O |
|-------|----------------|--------------------|------------|-----------------|------------------------|------------|-----|
| `susi-abi` | Swarm OS ABI: AI syscalls, stigmergic pheromones, evidence receipts, wire framing; optional `cell-server` TCP loop | `SyscallOp`, `SwarmPheromone`, `ToolReceipt`, `WireFrame`, `cell_server` (feature) | std, serde, serde_json; tokio behind `cell-server` | all workspace crates | no | no | cell TCP when featured |
| `susi-paths` | Host path/port contract and its service-backed client (served on `:18080` by `susi-leaf-services`); XDG / substrate paths + host-contract ports, both host bearer policies | `SusiDirs` (service with local fallback), `ports` (host contract 9090–9094, gossip 9095, and the leaf-service defaults `PATHS_SERVICE`…`NATIVE_SERVICE` 18080–18084), `loopback` (the one std-only loopback HTTP/1.0 client every leaf-service client uses: `Endpoint`, `request`, `service_port`, `local_env_override`), `local_paths_json`/`ports_json`, `host_token`/`with_bearer`/`bearer_authorized`/`supervisor_bearer_authorized` | std, serde_json (per-OS user dirs are a std-only `xdg` module) | everything else | no | no | reads env for XDG |
| `susi-error` | Stable error model + metrics sink (served on `:18081` by `susi-leaf-services`) | `EaiError`, `EaiResult`, `rewrap`, `ResultExt` (`context`/`with_context`, kind-preserving), `eai_err!`/`eai_bail!`, metrics sink, offset-aware service-backed event reporter (`service_port`), `append_posted_event`/`recent_error_entries` | `susi-paths`, std, serde, serde_json | candle, HTTP, feature crates | no | append-only metrics file | yes (metrics) |
| `susi-core` | Domain + ports (kernel ABI): Evidence, Truth, Provider, Tool, CapabilityRegistry, **`plane_bus`** facades + `plane_bus_ipc` rendezvous | traits + ledger types + `plane_bus::{gemi,gawd,tools,agents}` | `susi-abi`; `susi-paths`, `susi-error`, `susi-config`; `susi-adapters-llm` (re-export `inference_wire`); `susi-http-transport` (MCP session client); std, serde, concurrency libs | feature planes; HTTP client crates | providers/tools via registry | process registry | receipt archive paths |
| `susi-vendor-wasmer` | The one wasmer crate: WASI reflex host for the `susi-native` leaf service (`:18084`, HTTP shell in `susi-leaf-services`) + the metered, memory-capped in-process cell runtime and plugin-module validation the daemon uses | `wasm::WasmHost`, `cell::{WasmCell, spawn_wasm_cell, validate_module}` | `susi-error`; wasmer, wasmer-wasix, wasmer-middlewares, wasm-encoder | feature planes (they use `susi-native-client`) | Wasm modules | instance | yes |
| `susi-native-client` | First-party typed IPC client for `susi-native` | client `WasmHost` | `susi-error`, `susi-paths`, serde_json | Wasmer, feature planes | no | no | loopback HTTP |
| `susi-config` | `SusiConfig` + extension packs + versioned JSON store (served on `:18082` by `susi-leaf-services`) | `SusiConfig`, `extensions`, `VersionedJsonStore`, `enter_service_mode` | `susi-paths`, `susi-error`; serde, cluster-key crypto | HTTP clients, everything above paths/error | no | config files | yes |
| `susi-sandbox` | Docker (bollard) integration for the `:18083` leaf service (HTTP shell in `susi-leaf-services`); re-exports the client's helpers | `execute_in_docker`, re-exported `SandboxManager`, `manager` | `susi-error`, `susi-config`, `susi-sandbox-client`; bollard, tokio (time) | feature crates | Docker optional | config files | yes |
| `susi-dsh-cell` | Swarm cell binary wrapping the DeepSeek Harness CLI (`dsh --prompt`) as `inference`/`dsh` capabilities on the shared `susi-abi` cell loop; every syscall requires `SUSI_CELL_TOKEN` | binary only | `susi-abi` (`cell-server`); tokio | workspace crates | no | no | yes (spawns `dsh`) |
| `susi-universal-cell` | Swarm cell binary that serves any ecosystem plugin described by a JSON manifest (role → `SwarmRole`, capabilities, command) on the shared cell loop; spawned by daemon auto-discovery for executable plugins | binary only | `susi-abi` (`cell-server`); serde, tokio | workspace crates | no | no | yes (spawns the plugin) |
| `susi-leaf-services` | The one axum crate for the five leaf services: runtime/bind helper, both bearer policies as middleware, per-service routers, `service-run` dispatch, and the `susi-{paths,error,config,sandbox,native}` binaries (ports from `LEAF_SERVICES`) | `serve(name, port)`, `run_standalone(name)` | `susi-paths`, `susi-error`, `susi-config`, `susi-core`, `susi-sandbox`, `susi-sandbox-client`, `susi-vendor-wasmer`; axum, tokio | feature planes | no | no | yes |
| `susi-sandbox-client` | Sandbox IPC client + shared helpers: signed audit chain, daemon-state integrity, `SandboxManager` | `SandboxManager`, `audit_chain`, `daemon_state`, `manager` | `susi-paths`, `susi-error`, `susi-config` | bollard, feature crates | no | audit log, daemon state | loopback HTTP |
| `susi-http-transport` | Shared TLS-sniffing HTTP accept + Hyper connection builder + outbound timeout-bounded `ureq` (crate-private) via `http_call` / `http_call_with_body` / `http_post_utf8` | `dual_transport`, `http_conn`, `http_call`, `http_post_utf8`, `HttpCall::into_utf8` | tokio, tokio-rustls, hyper-util, ureq | all workspace crates; vendor SDKs | no | no | sockets |
| `susi-vendor-candle` | Candle / CUDA / Metal vendor substrate: device probe + Qwen2 GGUF split + Hugging Face tokenizers | `device`, `qwen2_split`, re-exported `candle_*` / `tokenizers` | candle-core, candle-nn, candle-transformers, tokenizers | all workspace crates | no | process device cache | GPU FFI via Candle |
| `susi-adapters-llm` | SUSI-authored LLM provider wire (not vendored SDKs): OpenAI / Anthropic / Gemini / Triton bodies, extractors, `InferenceProtocol`, and `post_json` | `inference_wire` | serde_json, `susi-http-transport` | all workspace crates; ureq/reqwest | no | no | HTTP via transport |
| `susi-tools` | Tool registry + MCP client adapters + `plane_handler` | `ToolRegistry`, `EngineHooks`, bus handler | `susi-core` (registry/capture/mac_policy over the bus rendezvous); `susi-sandbox-client`; `susi-native-client`; `susi-vendor-mcp` (MCP client; lease/handshake budgets from `SusiConfig` in `mcp_budget`) | workspace crates except core/sandbox/native clients; rmcp/reqwest (owned by `susi-vendor-mcp`); peers via `plane_bus` | tools | registry | yes |
| `susi-agents` | Agent meta registry + `plane_handler` over the external-agent surface; domain types live in core | registry, plane_handler | `susi-core` (registry/task_manager/agent_types over the bus rendezvous); `susi-config`; `susi-sandbox-client`; `susi-vendor-agents` (re-exported as `susi_agents::external`) | feature planes; peers via `plane_bus` | peers | registries | yes |
| `susi-vendor-agents` | Third-party agent integrations: external task executors + framework adapters (aider, autogen, crewai, openhands, e2b, gemini-cli, n8n, temporal, …) — durable run management, setup/status/doctor, process+python adapters | `AgentManager`, `catalog`, `definition`, `resolve_managed`, `redact`, `RunStatus` | `susi-paths`, `susi-error`, `susi-config`, `susi-core` (bounded_cmd, mac_policy egress), `susi-sandbox-client`, `susi-http-transport` | external agent CLIs/SDKs; bounded HTTP for cloud agent APIs | yes (egress-gated) | run dirs under config dir | yes |
| `susi-gemi-models` | Model select / provision / lifecycle (provider catalogs live in `susi-vendor-models`, re-exported) | lifecycle, coding_models, hardware, model_cache | `susi-core`; `susi-config`; `susi-sandbox-client`; `susi-vendor-candle` (device / GGUF inspect / tokenizers); `susi-vendor-models`; `susi-http-transport` (downloads) | gemi engines crate; peer feature crates | catalogs | cache dirs | yes |
| `susi-vendor-models` | Third-party model-provider integrations: cloud catalogs, frontier registry, OpenRouter, open-weight registry, Hugging Face discovery; owns `set_selected_model_override` (the `selected_model_override.txt` write contract) | `cloud`, `frontier`, `openrouter`, `open_weight`, `hf_discovery`, `set_selected_model_override` | `susi-paths`, `susi-error`, `susi-config`, `susi-core`, `susi-sandbox-client`, `susi-http-transport` | provider HTTPS APIs (bounded, egress-gated) | yes (egress-gated) | provider config dirs | yes |
| `susi-vendor-web` | Public web API integrations: Open-Meteo weather, DuckDuckGo instant answers (zero-key live evidence) | `live_search` (`gather_live_evidence`, `fetch_open_meteo_weather`, `fetch_duckduckgo_instant`) | `susi-paths`, `susi-error`, `susi-core` (mac_policy egress), `susi-http-transport` | open-meteo.com, api.duckduckgo.com | yes (egress-gated) | no | no |
| `susi-gemi` | Inference adapters (HTTP, MCP-as-provider) + SUSI InferenceHost | providers, engines, `plane_handler` | `susi-gemi-models` + `susi-abi`, `susi-core`, `susi-config`; `susi-sandbox-client`; `susi-vendor-candle` | peer feature planes | providers | model weights | yes |
| `susi-gawd-agents` | Fleet, safety/security, peers | agents, detectors, `plane_handler` topics via agents crate | `susi-core`; `susi-config`; `susi-sandbox-client`; `susi-vendor-web` (live evidence); `susi-http-transport` (peer HTTP) | peer feature planes; reqwest/ureq | agents | mission-local | yes |
| `susi-gawd-swarm` | AMA / DAG / cloud recovery; verified cluster-peer RTT is measured from signed ping/pong nonces (uptime remains unknown) | swarm dispatch | `susi-gawd-agents` + `susi-core`, `susi-config`; `susi-sandbox-client` | peer feature planes; HTTP clients | no | blackboard | yes |
| `susi-gawd-a2a` | A2A (`ra2a`) wire | task store, executor | `susi-gawd-agents` + `susi-core` + `susi-http-transport` | peer feature planes; reqwest/ureq | transport | tasks | yes |
| `susi-gawd` | Host facade: admin, evolution, reflex synth | re-exports + host modules | `susi-gawd-{agents,swarm,a2a}` + `susi-abi` + `susi-core` + `susi-native-client` | peer feature planes except the dedicated native client; ra2a/reqwest/axum | no | genome/reflexes | yes |
| `susi-gmcp` | MCP HTTP/stdio server + core tools | MCP surfaces, `plane_handler` via tools/agents/gawd bus | `susi-abi`; `susi-core` (plane_bus/intent_bus/agent_tx/mac over the bus rendezvous); `susi-config`; `susi-sandbox-client`, `susi-http-transport`, `susi-vendor-mcp-server` (rmcp server SDK); optional `susi-vendor-{chrome,tantivy,fastembed}` (`tools-rich`) | peer feature planes; swarm/admin via `plane_bus::gawd` / `gawd_hooks` | MCP servers | sessions | yes |
| `susi-vendor-mcp` | The MCP client SDK (rmcp) + its reqwest streamable-HTTP transport: pooled connections, connect-failure cooldown, lease-bounded blocking `list_tools_blocking` / `call_blocking_result` over `McpServerConfig` | `McpServerConfig`, blocking calls | rmcp, reqwest, tokio | workspace crates | MCP servers | connection pool | no |
| `susi-vendor-mcp-server` | The MCP *server* SDK (rmcp): Streamable HTTP + stdio, `#[tool]` macro, protocol types. GMCP depends on this crate and never declares `rmcp` | re-export of server `rmcp` | rmcp (server features) | workspace crates; `susi-gmcp` | MCP clients | sessions | no |
| `susi-vendor-tantivy` | Full-text `DocIndex` (doc_id/source/content): add, replace, BM25 search, get, enumerate | `DocIndex`, `DocWriter`, `StoredDoc` | `susi-error`; tantivy | workspace crates | no | index dir | no |
| `susi-vendor-fastembed` | One process-wide fastembed model (ONNX) behind `embed` / `embed_one`; cached load failure is visible to provider health | `embed`, `embed_one`, `load_failed` | `susi-error`; fastembed (hf-hub + rustls ORT download) | workspace crates | no | model cache | no |
| `susi-vendor-syn` | Rust source analysis for the bloat auditor, reflex validation and the `ast_analyze` tool | `is_valid_rust`, `analyze` → `RustMetrics`, `outline` → `(ItemKind, name)` | syn | workspace crates | no | no | no |
| `susi-vendor-chrome` | Headless Chrome page capture (PNG + DOM) for an already-validated URL | `capture_page`, `PageCapture` | `susi-error`; headless_chrome | workspace crates | Chrome/Chromium | no | no |
| `susi-vendor-cloud` | Provision/manage local+cloud hosts via operator CLIs (no AWS/GCP/K8s SDK). Tick = `probe_all`; list/apply only via `susi os provision` | `probe_all`, `list_nodes`, `apply_manifest`, `CloudKind::parse` | std `Command` | `susi-daemon`, root CLI | kubectl/docker/aws/gcloud/az | no | no |
| `susi-server` | Hyper HTTP adapters for GEMI REST | bind helpers | `susi-core` (plane_bus facades over `IpcPlaneBus`, file-backed broker, context graph bound to the shared workspace JSONL); `susi-paths`, `susi-error`, `susi-config`; `susi-sandbox-client`; `susi-http-transport` | peer feature planes; GAWD/GEMI via `plane_bus` | no | — | yes |
| `susi-daemon` | Persistent host: per-peer HMAC gossip 9095 + sealed (fail-closed without a MAC key) `gossip_caps.json`, egress-gated STUN / TURN MESSAGE-INTEGRITY, live OS-plane ticks (cloud **version-probe** only), `os_planes.json` | `SusiDaemon`, `composition`, `gmcp_bootstrap`, `swarm_host_snapshot`, `wire_daemon_os_planes` | **all** feature crates + `susi-vendor-cloud` + `susi-abi` + server + tools + agents; `susi-http-transport` (webhooks) | HTTP client crates | no | lock/PID + host dirs | yes |
| `susi` (root) | CLI + composition entry for workspace intents | `main`, CLI modules | daemon + feature crates + `susi-leaf-services` (`service-run`) + leaf `susi-paths`/`susi-error` (real Cargo deps) | — | — | cwd workspace | yes |

Workspace crate cycles must remain **zero**. Feature planes have **zero Cargo
peer dependencies** on each other (no `susi-gemi` ↔ `susi-gawd` ↔ `susi-tools`
↔ `susi-agents` ↔ `susi-gmcp` ↔ `susi-server` edges). They communicate only
through **`susi_core::plane_bus`** (topics + JSON DTOs) and shared foundation
(`susi-paths`, `susi-error`, `susi-config`, `susi-sandbox-client`,
`susi-native-client`, `susi-core` and `susi-abi` as real Cargo dependencies).
**`susi-daemon`** and the root **`susi`** package register `plane_handler`
implementations and may link every plane.

Every SUSI Cargo edge points down the leaf order in `AGENTS.md`
(`paths → error → config → core/sandbox → services → daemon`);
`tests/architecture_tests.rs` (`workspace_edges_follow_the_leaf_order`) holds
the rank table. Shared code is being moved from `#[path]` mounts to crates:
`susi-paths`, `susi-error`, `susi-config`, `susi-sandbox-client`,
`susi-core` and `susi-abi` are consumed as real crates
(`SusiDirs` asks the paths service and falls back to the same local resolver
the service answers with; every crate returns the one `susi_error::EaiError`,
whose events post to the error service with a local-file fallback; the global
`SusiConfig` read/write prefers the config service and falls back to the
local files; `susi-sandbox-client` owns the sandbox IPC client and the
audit-chain / auto-install / daemon-state / manager helpers the service and
every client share, so bollard stays linked only by `susi-sandbox`;
`susi-core` and `susi-abi` are each one compiled copy, re-exported by their
consumers), and a ratchet
(`cross_crate_source_mounts_only_decrease`) keeps the remaining cross-crate
mounts from growing.

Shared code that is still compiled into each consumer through `#[path]`
(pending conversion to a crate edge): **none**. The
`cross_crate_source_mounts_only_decrease` ratchet ceiling is 0.

**`susi-core` is a Cargo dependency, not a mount.** Every plane declares
`susi-core = { workspace = true }` and re-exports it (`pub use susi_core;`),
so `crate::susi_core::<module>` resolves to the one compiled copy;
`tests/duplication_tests.rs`
(`susi_core_is_never_source_mounted_into_a_consumer`) makes a regression of
the old `#[path]` mount a hard failure.

**`susi-abi` is a Cargo dependency, not a mount.** Planes and cell binaries
declare `susi-abi = { workspace = true }` and re-export it
(`pub use susi_abi;`); the five cell binaries enable the `cell-server`
feature for the shared TCP loop. `susi_abi_is_never_source_mounted_into_a_consumer`
makes a regression of the old `#[path]` / `embedded.rs` mounts a hard failure.

**`susi-gawd-agents` is a Cargo dependency, not a mount.** The swarm, A2A
and host crates declare `susi-gawd-agents = { workspace = true }` and
re-export the fleet/detector modules so `crate::safety` / `crate::agents`
keep resolving; `susi_gawd_agents_is_never_source_mounted_into_a_consumer`
makes a remount a hard failure.

**`susi-gawd-swarm` / `susi-gawd-a2a` / `susi-gemi-models` are Cargo
dependencies, not mounts.** The host `susi-gawd` crate depends on the swarm
and A2A crates and re-exports their modules; `susi-gemi` depends on
`susi-gemi-models` and re-exports it as `models`.
`within_plane_crates_are_never_source_mounted_into_a_consumer` makes a
remount a hard failure.

**`susi-http-transport` is a Cargo dependency, not a mount.** GEMI REST,
GMCP and A2A share one TLS-sniffing accept loop and one Hyper connection
builder; MCP/peer/search callers share `http_call` / `http_call_with_body`
and never name `ureq::Agent`. Vendor SDKs and provider wire shapes stay out.
`susi_server_transport_is_never_source_mounted_into_a_consumer` makes a
remount a hard failure. `core_os_crates_must_not_declare_http_clients`
forbids `reqwest`/`ureq` on paths/error/config/gawd host/swarm/agents/a2a.

**`susi-vendor-candle` is a Cargo dependency, not a mount.** Candle,
CUDA, Metal and the Qwen2 GGUF split live in one rank-2 crate so
`susi-gemi` and `susi-gemi-models` share one feature graph and one
process-wide device cache;
`susi_vendor_candle_is_never_source_mounted_into_a_consumer` makes a
remount a hard failure.

**`susi-adapters-llm` is a Cargo dependency, not a mount.** OpenAI /
Anthropic / Gemini / Triton request bodies and extractors are
SUSI-authored adapter code (not a vendored SDK) and live in their own
rank-2 crate; `susi-core` re-exports `inference_wire` so existing
`susi_core::inference_wire` paths keep resolving.
`susi_adapters_llm_is_never_source_mounted_into_a_consumer` makes a
remount a hard failure.

One copy per *process*, not per crate: the daemon and each cell binary
(`susi-gawd`, `susi-gemi`, `susi-gmcp`, `susi-dsh-cell`,
`susi-universal-cell`) run in their own address spaces, so
`PlaneBus::global()` / `CapabilityRegistry::global()` statics are still
per-process. `plane_bus` is therefore backed by
**`plane_bus_ipc::IpcPlaneBus`**: a filesystem rendezvous under
`<cache>/bus/<pid>/` (endpoint files for exact topics and prefixes) plus a
lazily-bound `127.0.0.1:0` listener serving `POST /handle` and
`POST /stream`. Stream ids embed the opener's endpoint (`ipc://<addr>/<id>`)
so `stream_emit` routes cross-process; stale endpoint files are pruned on
connect failure and dead pid dirs swept via `/proc`. Writes stay scoped to
the owning process's pid dir (ownership/liveness), while **reads scan every
numeric-named sibling pid dir** — own dir first, then sorted siblings — so
separate plane processes resolve each other's registrations through the
same `bus/` root. Exact topic registrations outrank prefix handlers
process-wide.

**`registry_ipc::IpcCapabilityRegistry`** applies the same pattern to the
capability catalog: `register_tool` keeps the trait object local, serves
`capability.tool.<name>` on the owner's bus (MAC + evidence capture run in
the owner-side dispatch handler), and writes metadata under `caps/`; lookups
in another process return a `RemoteTool` / `RemoteProvider` proxy that
forwards `execute`/`generate`/`embed` over the bus. Agent capabilities are
pure data — `caps/agent/` files only.

The rest of the microkernel state follows the same file-backed rendezvous so
processes agree: `IpcBroker` under `<cache>/bus/<pid>/broker/` (grants,
requests and inboxes as JSON files, so CLI and daemon broker state
interop), `capture`/`evidence`/`receipt_archive` with a receipts inbox
drained by the owning session, `mac_policy` sharing `~/.susi/mac.hmac.key`,
`intent_bus` (providers/needs persisted under the rendezvous, scanned across
pid dirs), `agent_tx` (durable `.susi/tx/` journal, workspace-shared),
`telemetry` history, and `service_table` at
`substrate_home/services.json`. `GemiServer::start_http_server` binds
`ContextGraph` to `<workspace>/context_graph.jsonl` — the same log the
daemon binds, and every read path already `replay()`s it.

Within-plane Cargo edges remain allowed: `susi-gemi` → `susi-gemi-models`;
`susi-gawd` → `susi-gawd-{agents,swarm,a2a}`; `susi-gawd-swarm` →
`susi-gawd-agents`; `susi-gawd-a2a` → `susi-gawd-agents`.

**Process layer.** `susi-core::service_table` is the kernel ABI for the
leaf services: the `LEAF_SERVICES` registry (binary name, port env var,
default port), the shared process table at `substrate_home/services.json`
(atomic temp+rename writes; corrupt reads as empty), `probe` (TCP liveness),
`pid_alive`, and `status` (registry joined with live probes).
`susi-daemon::supervisor` is the init half: `ensure_leaf_services` +
`monitor_loop` spawn any leaf service whose port is dead and record
supervised pids — resolution order is a sibling `susi-<name>` binary
(dev/staged layout) then self-reexec of the running `susi` binary in
`service-run <name>` mode, so a staged `~/.susi/bin/susi` is always able
to fill every leaf role with a version-matched process. Each leaf crate
exposes `serve(port)` in its lib; the standalone `susi-<name>` binaries
are thin shells over the same entry. Health checks run on a 5s cadence;
services missing from the table are retried every 30s (binary may
appear after daemon boot); crashes respawn up to 10 restarts then
suspend for a 5min `disabled_until` cooldown (fresh budget after) —
never abandoned. SIGTERM→SIGKILL applies to the daemon's own children
only — it never kills processes it did not spawn. A port already bound
by a foreign process (a `cargo run` dev binary, a manually launched
service) is recorded as an `external` table row — `pid_for_port`
resolves the holder via `/proc/net/tcp` inode → fd scan on Linux;
external rows are health-probed by port only, never signaled, and when
the external process dies the supervisor drops the row and spawns a
managed service on the freed port.
`susi services` (root CLI) prints the joined table/health view
(external rows marked `*`) and `susi services restart <name>` SIGTERMs
a supervised pid for the supervisor to respawn — refusing external
records, which are not ours to signal. `susi-gmcp` exposes the same
surface to agents as
governed tools (`os_services`, `os_ps`, `os_sysinfo`, `os_kill`), each
behind `gawd_hooks::audit_action`; `os_kill` additionally fails closed to
pids present in the process table only, so the tool can never signal an
arbitrary host process.

Hook traits (`EngineHooks`, `AdminHooks`, `HostHooks`, `dag_hooks`) remain for
composition-root wiring alongside the bus — never by stuffing implementations
into `susi-core` beyond the bus facades.

## Ports (traits) — create only at real substitution boundaries

| Port | Home | Implementations |
|------|------|-----------------|
| `Provider` | `susi-core` | Candle, HTTP OpenAI-compat, MCP-as-provider |
| `Tool` | `susi-core` | Core tools, MCP tools, observed wrappers |
| `CapabilityRegistry` | `susi-core` | process-wide catalog (locator today; prefer pass-by-ref in new code) |
| `ContextGraph` | `susi-core` | process-wide evidence-backed entity/relation graph; storage path supplied by composition root |
| `IpcBroker` | `susi-core` | inter-app permission grants, negotiation, and typed message passing |
| `PlaneBus` | `susi-core` | in-process request/reply between feature planes (topics + JSON); handlers registered at composition roots |
| `MacPolicy` / `CapabilityToken` | `susi-core` | HMAC-SHA256 capability MAC on every tool dispatch; privacy modes |
| `IntentBus` | `susi-core` | semantic provider/need matching (Jaccard + hash embeddings) |
| `TxManager` | `susi-core` | multi-agent file/blackboard transactional snapshots |
| `TelemetrySnapshot` | `susi-core` (types) / `susi-gemi::telemetry` (Linux sampler) | thermal zones, battery, load; daemon watchdog consumes |
| `EngineHooks` | `susi-tools` | `susi_daemon::engine_hooks::SusiEngineHooks` |
| `AdminHooks` / `HostHooks` | gawd-agents / gawd-swarm | gawd host facades (includes `apply_patch_cycle`) |
| `ProtocolDispatcher` / `CapabilityResolver` | `susi-gmcp` | MCP protocol |

Do **not** invent empty interfaces for every struct. Prefer the existing trait
surfaces and extension-pack capability strings.

## Composition roots

Exactly two process-entry assembly paths:

1. **CLI** — `susi_daemon::composition::wire_cli_substrate` (from `src/main.rs`)
   then command dispatch against cwd workspace.
2. **Daemon** — `SusiDaemon::run` → `wire_engine_hooks` →
   `bootstrap_zero_config_substrate` → bind host-contract ports 9090–9094.

Sequence (both paths, subset as applicable):

1. Register `plane_bus` handlers (`composition::wire_plane_bus`) for every
   feature plane.
2. Load / seed configuration and extension packs.
3. Init hook traits (`EngineHooks`, and GAWD hooks on first swarm use).
4. Apply secrets surface (`~/.susi/cloud.env`) — never log secret bodies.
5. Construct / discover infrastructure (providers, MCP, peers).
6. Mount capabilities into `CapabilityRegistry`.
7. Initialize `ContextGraph` durable storage path (`~/.susi/context_graph.jsonl`).
8. Attach transports (CLI ack, HTTP, MCP, UDP discovery).
9. Start / serve.

Business / domain code must not construct global `HttpClient`, DB pools, or
plugin managers inside use cases.

`CapabilityRegistry::global()` is a **documented service locator** for
zero-config (Mandate 44). New call chains that already hold a registry
reference should pass it explicitly.

## Plugin triad (SUSI-shaped)

| Surface | Role | Manifest / discovery |
|---------|------|----------------------|
| **Extension pack** | Vendor opinions + capability declarations | `~/.susi/extensions/<id>/manifest.json` |
| **MCP / peer admit** | External tools and agents | `mcp_config.json`, catalogs, `CapabilityRegistry` |
| **Wasm reflex** | Sandboxed synthesized skill | Wasmer under data dir |

Pack manifests support (additive, Mandate 35 flatten for unknowns):

- `id`, `name`, `version`, `apiVersion`
- `capabilities`, `requires`, `optional`, `permissions`
- `files` map for catalogs

Loading: discover → validate manifest → check `apiVersion` major → resolve
requires → mark loaded. Optional pack failure must not stop the daemon.

Permissions are **enforced**: `validate_manifest` rejects unknown permission
strings and `files` entries that escape the pack root without `filesystem.read`
(absolute paths are always rejected); `resolve_pack_path` applies the same jail
at resolution time as defense in depth. Never treat discovery as trust.

## Errors

`EaiError` variants stay SUSI-shaped (`Governance`, `Inference`, `Sandbox`, …).
Every variant exposes:

- `kind_name()` / `code()` — stable machine code
- `retryable()` — whether a caller may retry
- human `Display` message
- backtrace for logs only

Do not leak candle/provider exception types across crate boundaries; convert at
the GEMI (or other adapter) edge.

## Configuration

- Dynamic flatten registries (Mandate 35) for user-editable JSON.
- Host-contract ports are compile-time constants in `susi-paths` (not overridable).
- Prefer `SusiConfig` accessors over scattered `std::env::var` in new code;
  env remains valid for override knobs (`SUSI_*`).

`SusiConfig` and the dynamic-registry substrate (typed config fragments,
extension packs, `VersionedJsonStore`) live in the `susi-config` leaf REST
service crate, which every consumer depends on (IPC + local fallback).
`susi_sandbox_client::manager` re-exports that surface so existing
`susi_sandbox::manager` import paths keep resolving.

## Architecture tests

`tests/architecture_tests.rs` asserts:

- No workspace crate dependency cycles.
- Every SUSI edge points down the leaf order (`LEAF_RANK`).
- Forbidden edges (notably `susi-core` / `susi-error` purity and **zero
  peer deps between feature planes** — communication via `plane_bus` only).
- Layer matrix from this document.
- `ARCHITECTURE.md` documents `plane_bus`.
- No new unwired `susi-daemon` module: modules unreachable from any production
  path are a ratchet with ceiling **34** (`UNREACHABLE_DAEMON_MODULES_CEILING`).
  Wire from `composition` or remove; discarded probes and type-only references
  do not count as production wiring.
- No new cross-crate `#[path]` mount: 159 existed on 2026-09-28; the count
  may only decrease (142 after `susi-paths` became a crate dependency, 125
  after `susi-error`, 105 after `susi-config`, 94 after
  `susi-sandbox-client`).

`tests/duplication_tests.rs` rejects byte-identical Rust files and physical
copies of shared contracts inside consumer crates.

CI runs the full workspace test suite (includes these tests) and `cargo deny`.
Criterion benches live under `crates/susi-core/benches/`; nightly fuzz targets
under `fuzz/` (stable smoke in `tests/fuzz_smoke.rs`).

## Definition of done (SUSI)

Close to the modularization goal when:

- [x] Domain (`susi-core`) has no networking / Candle / Wasmer / Docker deps.
- [x] External systems reached through ports (`Provider`, `Tool`, hooks).
- [x] Concrete wiring only at CLI/daemon composition roots.
- [x] Features admit via registries / config / packs — not central `if type ==`.
- [x] Packs use versioned manifest fields + SDK triad above.
- [x] Optional pack validation failure does not abort substrate seed.
- [x] Modules testable; architecture tests in CI.
- [x] No workspace dependency cycles.
- [x] Pack permission enforcement at manifest load + file resolution.
- [x] `SusiConfig` relocated out of sandbox (→ `susi-config` crate).
- [x] Versioned application event schemas beyond the existing bus
  (`susi_core::bus::SwarmEvent` envelope: `schema_version` + `#[serde(flatten)]`
  v1 `SwarmEventType` payload; `SwarmEvent::decode` drops unknown versions with
  a warning, never panics).

Public boundaries use stable DTOs and versioned events where applicable.
Configuration authority for bundled JSON is documented in
[`config/README.md`](config/README.md).

`susi-gmcp` defaults to feature `tools-rich` (browser / tantivy / qdrant /
fastembed / syn AST tools). Use `--no-default-features` on that crate for
lighter local checks; shipped binaries keep the default.

## Autonomy surfaces (substrate-level)

These close the gap between “agent-of-agents substrate” and multi-hop /
cross-app / self-healing operation — still workspace-confined and
evidence-gated, not a kernel IPC or power-management daemon:

| Surface | Entry points | Behavior |
|---------|--------------|----------|
| **Autonomous planning loop** | `susi plan`, `SusiMasterAgent::solve_autonomous` | Decompose goal → multi-step mission pipeline → abort on failure (earlier steps' workspace changes are reported as not rolled back) → synthesize |
| **Inter-app permission / IPC broker** | `susi broker`, MCP `ipc_*`, HTTP `/broker/*` | Grant / request / negotiate / dispatch messages between identities |
| **Host telemetry** | `susi telemetry`, MCP `host_telemetry`, HTTP `/telemetry`, daemon watchdog | Linux thermal / battery / load; throttles fleet concurrency under stress |
| **Apply-patch-then-test** | `susi patch`, MCP `apply_patch_cycle`, HTTP `/patch/apply`, `SelfHealingAgent` via `AdminHooks` | Workspace-confined edit → test → rollback on failure; gated by `trust_level` / `auto_apply` |
| **MAC + edge privacy** | `susi privacy`, MCP `privacy_*`, `MacPolicy` on every tool | HMAC capability tokens; `local_only` blocks cloud inference + network egress unless consented; mandatory Docker sandbox for host exec |
| **Semantic intent bus** | `susi intent`, MCP `intent_*`, fleet recruitment | NL/hash-embedding provider↔need matching; publishes `IntentRouted` on typed bus |
| **Ambient context sync** | `susi ambient`, daemon ambient indexer | FS mtime poll → ContextGraph + SemanticIndex refresh |
| **Multi-agent transactions** | `susi tx`, MCP `tx_*`, patch/plan loops | File (+ optional blackboard) snapshots with commit/abort restore |

## The learning loop

The substrate's intelligence is a loop — `perceive → retrieve → deliberate →
act → verify → distill → promote` — not a single model. The loop's unit of
experience is the **mission trace** (`susi_core::mission_trace::MissionTrace`,
schema v1): goal (bounded + credential-redacted), outcome verdict, the route
that produced it (`swarm` / `fast-path` / `governance-block`), tools and
agents consumed, evidence count, and wall-clock seconds from the evidence
session. `SusiMissionReport::persist_inspectable_trace` is the terminal
choke point — every mission that reports an outcome emits one trace to three
sinks:

- `<workspace>/.susi/mission_traces.jsonl` — append-only, `FileLock`-guarded;
- `ContextGraph` — an outcome observation linked to the mission node;
- `<workspace>/.susi/distillation_staged.jsonl` — via
  `ProtocolKnowledgeBase::stage_distillation_pair`, feeding Tier-0
  (`SusiAlphaModel`) training and `reflex_trainer`.

`mission_trace::read_all` tolerates older-schema and partial lines, so trace
consumers never break on a rolled-forward file.

The `verify` stage runs through the **contract registry**
(`susi_core::verification`): `Contract::{FileExists, FileAbsent,
FileContains, FileHash, CommandExit}` evaluate against physical workspace
state and return evidence (observed path, hash, exit code), not bare
booleans. `CommandExit` runs through `bounded_cmd::output_within` with a
workspace cwd, a ≤60s deadline, and a per-program argv policy
(`verifier_policy`) — a verifier is a probe: pure readers (`grep`, `cmp`,
`diff`, `test`, `cat`, ...) with any arguments, `git` only with read
subcommands (`status`/`diff`/`log`/`show`/`rev-parse`/`ls-files`), `cargo`
only `test`/`check`/`build`/`clippy`/`fmt --check`, and no shells. Refused
argv (or unresolvable claims) return `Unverifiable` before any process
spawns, never `Verified`. Until EV-CLAUDE-019, `sh`, `bash`, `git` and
`cargo` were allowlisted with any arguments, so `sh -c "rm -rf ."` was a
valid "probe". `verify_mission_reality`
mines goal and result text into contracts: write *and* delete claims are
checked, and a goal's `containing <text>` clause upgrades existence to a
content assertion.

The `retrieve` stage consults history before planning
(`susi_core::mission_trace::{similar, history_brief, difficulty}`): traces
most similar to the goal (token-Jaccard ≥ 0.15, stopword-filtered) inject a
"prior outcomes" brief into the decomposition prompt, and a `Difficulty`
estimate — novelty, similar-mission failure rate, manifold risk — decides
routing: `demands_deliberation()` widens the candidate search *and* raises
the model floor — `solve_internal`'s first inference attempt gets a
`Moderate` min-complexity for novel/historically-failed intents instead of
paying a failed attempt to learn it — while familiar reliably-solved
intents stay on the cheap path.

The `deliberate` stage is plan search (`susi_gawd_swarm::deliberation`):
`solve_autonomous` no longer commits to the first decomposition. Candidates
are generated at several step budgets, scored purely (goal-token coverage
+0.5·coverage, risk-vocabulary −0.15/token, verifiable steps +0.05, over-
budget −0.10), sorted best-first, and every rejected candidate's score and
rationale lands in the mission record (`PLAN_SEARCH` interaction) and the
printed omni-trace. Mutate/SelfExtend intents and High+ risk demand
**consensus** — the top two candidates must reach a step-token Jaccard ≥
0.35 or the mission declines multi-step autonomy in favor of the
single-step goal. On step failure, Read-scope goals fall through to the
next candidate (reads mutate nothing); mutating scopes abort rather than
re-run a guess over changed state. Retrieval also feeds scoring, not only
the prompt: `mission_trace::failing_tools` extracts tool tokens that only
ever appeared on *failed* similar traces, and `score_plan_weighted` docks
each mention −0.10 (`failed_history_tools` in the rationale) — a plan that
repeats a known-failing tool is outscored, not merely flagged.

The `promote` stage is governed reflex synthesis
(`susi_core::mission_trace::promotion_status` gating
`EvolutionManager::evolve_recurring_intent`): frequency alone no longer
earns a WASM reflex. A recurring intent must show ≥
`MIN_PROMOTION_SUCCESSES` (2) verified-success traces and no failure inside
its last 3 traces; a failure in the window `Veto`s promotion and flags the
intent as an anti-pattern — which also forces `consensus_required` in
deliberation. Intents with no trace history keep the legacy audit-log path;
intents observed but unproven `Defer`. Veto/defer decisions never consume
the 24h synthesis budget and are audit-logged (`EVOLUTION_REFLEX_VETO` /
`_DEFER`); promoted reflexes still compile-and-run in the WASI sandbox
before publishing.

The `distill` stage turns staged pairs into Tier-0 weights
(`susi_gawd::reflex_trainer` → `SusiAlphaModel::train_on_staged_file`).
Every writer of `distillation_staged.jsonl` — mission traces via
`ProtocolKnowledgeBase::stage_distillation_pair`, verified receipts via
`ReceiptArchive` — appends under the `distillation_staged` `FileLock`, the
same lock the trainer holds while it claims (renames) the buffer, restores a
failed claim, or recovers orphaned claims. Those three read the buffer and
atomically replace it, so an unlocked append in between would be written to
the replaced inode and lost.

Only successes are trainable. The classifier has no negative class, so a
failed mission's `goal → action` pair would teach the model to repeat the
failure. `SusiMissionReport` stages a pair only when `MissionTrace::succeeded`
(`SUCCESS`/`COMPLETE`), and `parse_training_entries` independently skips any
record whose `performance_metadata.outcome` is anything else — covering
samples staged before the source guard and any future writer. Receipt
samples carry no outcome (they come from successful tool calls) and stay
trainable.

Labels must agree. Receipts stage `mission goal → tool` once per tool call,
so a multi-tool mission stages one goal under several labels.
`resolve_label_conflicts` keeps an intent's samples only for the label
holding a strict majority of that intent's samples in the batch, and drops
them all when no label does. Repeated single-tool missions still teach;
multi-tool goals — which no single reflex action could serve — do not.

The action vocabulary has 128 slots (one per output). It is seeded with
the 9 foundational intents plus agents and tools *alphabetically*, and
before EV-CLAUDE-012 a full vocabulary never changed: a newly installed or
late-alphabet tool stayed "outside the vocabulary" and its samples were
skipped forever. `admit_staged_actions` now gives a staged action a slot
when it is a real capability (`capability_names`, untruncated): appended
while there is room, otherwise by reclaiming the highest-index slot that
is not foundational and has no training support (absent from the replay
set and the batch — trained only on its synthetic prime). Indices of every
other action are stable; the reused output row is refit in the same cycle,
and the report lists `old -> new` reclaims.

The classifier's inputs are `SusiAlphaModel::reflex_features`: a 128-dim,
L2-normalized bag of words with stopwords dropped, in which every word
contributes equal norm regardless of position — a category word ("status",
"read", "fix", ...) spread over its fixed 10-dim band, any other word over
two FNV-1a buckets. Fleet recruitment keeps its own
`semantic_centroid_projection` (category band at 1.0, other words one
bucket at 0.5, first word weighted most), which its 0.35 cosine threshold
is tuned to. Under that projection a leading category word decided
everything — "write a poem about the ocean" sat within 0.01 of "write
notes to todo.md" and was served `write_file` at 0.84 confidence
(EV-CLAUDE-006). Both projections hash with FNV-1a; until EV-CLAUDE-004
the hash was a byte *sum* that made every anagram one feature. Feature
changes need no checkpoint migration: the held-out gate below scores the
active and candidate weights under the *current* features, so stale
weights are judged honestly and replaced on the next cycle.

Fits run to convergence, not a fixed step count: full-batch AdamW until
mean loss ≤ 0.15 (correct class ≈ 0.86 probability) or 600 epochs, and the
report states both. The old fixed 100 steps, measured on 962 samples over
20 actions, reached 95% argmax accuracy with loss still 1.6 — 0% of samples
cleared the 0.5 serve confidence, so a grown Tier-0 was right and silent
(EV-CLAUDE-011). The loss is **class-balanced**: each staged sample
weighs `n / (K · n_c)`, so every action contributes equally however skewed
usage is; synthetic primes weigh 1 (a seed, not evidence). With one action
at 400 samples and nine at 6, plain mean loss met its target by fitting the
majority alone — over five inits, 1–11 of 36 unseen minority phrasings
were classified correctly and 13–28 were confidently served the *majority*
action; balanced, 34–36 correct and 0–2 wrong-but-served (EV-CLAUDE-015).

Training is cumulative *and* rehearsed. Each cycle fine-tunes the active
weights, and fine-tuning on only the claimed batch would overwrite what
earlier batches taught (catastrophic forgetting). So every cycle trains on
the claim plus the **replay set** — `<weights>.replay.jsonl`, the most
recent 2048 distinct intents (stored by action *name*, so they survive
vocabulary growth; newest label wins when an intent is restaged with a
different action). The replay set is rewritten only on the publish path (just before the
bundle), so a held-back cycle leaves it untouched.

Serving is gated on **support** as well as confidence
(`SusiAlphaModel::{support, predict_intent}`). The classifier has no
abstain class — every prompt maps to *some* action — so confidence alone
cannot say "I have never seen anything like this". A loaded model carries
the reflex features of its replay set; a prompt is served a Tier-0 reflex
only if its nearest trained intent is at cosine ≥ 0.6 (`SUPPORT_MIN`) *and*
confidence > 0.5. Everyday out-of-distribution prompts measured ≤ 0.52
against everyday training data, while a paraphrase sharing two of three
content words sits near 0.67. Checkpoints without a replay set (bootstrap,
pre-replay) keep the confidence-only gate, and so does a checkpoint whose
replay set cannot be read: support is a serving refinement, so an I/O
fault degrades Tier-0 (recorded in the error-metrics sink) instead of
failing the checkpoint load and disabling it. The replay file is written
*before* the bundle publishes, because the bundle's mtime keys the model
cache and a cached model must not carry a stale support set.

Publication is gated on held-out accuracy (`SusiAlphaModel::holdout_gate`).
One in five staged-or-replayed intents — chosen by a hash of the
normalized intent, so a sample lands in the same split every cycle — is
held out; replayed held-out samples make the gate a forgetting check too. A candidate fit
from the active weights on the rest must predict the held-out samples at
least as well as the active checkpoint does (the active model can only
score labels inside its own, older vocabulary) *and* must not make more
**wrong-but-served** predictions — wrong argmaxes above the 0.5 serve
confidence (`SERVE_CONFIDENCE`, shared with `predict_intent`), the
mistakes Tier-0 would actually hand out. Equal accuracy with more
confident errors is a worse reflex (EV-CLAUDE-016). A regression publishes
nothing and returns an error, so `ReflexTrainer` restores the claim and the
samples are retried with more data instead of being dropped. When the gate
passes, the published checkpoint is refit on *all* staged samples. The gate
is skipped — and the report says so — with no active checkpoint or fewer
than 3 held-out samples.

Every cycle is logged to `<workspace>/.susi/distillation_log.jsonl` (last
200: `published` / `held_back` / `error`, claim size, redacted report).
Training runs in the background after every supervised mission and its
result used to be discarded, so a held-back checkpoint was invisible. The
log is also back-off state: after `held_back`, the automatic audit defers
until at least `reflex_training_threshold` new samples arrive beyond the
held-back claim, instead of refitting and refusing the same restored claim
on every mission. A failed (`error`) cycle backs off the same
way, and a claim with *nothing* trainable (every line a failed outcome, a
non-capability label, or blank) is retired and logged `untrainable` rather
than restored: restoring it re-claimed and re-failed the same lines on
every mission while the buffer grew — a retrain livelock (EV-CLAUDE-017).
The staging buffer is bounded: a claim keeps the newest 20,000 valid
samples (`STAGING_CAP`), recording any drop in the error-metrics sink, so
even a persistently failing trainer cannot grow it without limit.
`force_train` (operator-requested) bypasses the back-off.
`susi substrate status` shows the log under `reflexes.distillation` (cycle
counts by outcome plus the last cycle). `tests/distill_loop_tests.rs` exercises the whole
stage end to end across the plane bus — production staging writer →
automatic trainer audit → GEMI training, gate and publication → Tier-0
serving through `SusiPulse` — under an isolated HOME/XDG root.
Lane ownership for concurrent brain work is in
`docs/brain-lanes.md`.

## Federation & consensus

Cross-node quorum decisions are durable, signed, and replicated — but this is
**not Raft/Paxos**: the ledger records signed *decisions*, not a replayable
operation log, and there is no cross-coordinator term ordering.

| Layer | Mechanism | Location |
|-------|-----------|----------|
| **Cluster membership** | HMAC-SHA256 signed ping/pong (`susi-peer-v1`) keyed by `~/.susi/cluster.key` (0600); the v2/v3 dialects (`SUSI_PING_SIG2` → `SUSI_PONG_SIG3` + `SUSI_PONG_SIG2`, v3 tried first by the scout and `peers add`/`probe`, falling back for pre-PKI peers) attest each node's Ed25519 `node.key` pubkey in the pong — v3 additionally carrying the subject-signed `bind_sig` attestation — binding member identity to a private key for record attribution. Each node has a persisted unique identity at `~/.susi/node_id` (`wire_node_id()` — signed into pings so seq chains, chain linkage, and elections live in distinct namespaces; a read-only substrate falls back to a per-process ephemeral id, never a shared constant). Verified peers persist to `~/.susi/peers.json` and rehydrate as `PeerAdmission::Explicit`. `Discovered` peers can never vote or lead. Operator eviction writes `~/.susi/peers_banned.json` — a banned member gets no handshake at all: its signed ping is verified then refused with no pong, so the ban severs trust bilaterally (it cannot verify us either). | `susi-gawd-swarm::peer_registry`, `susi_config::cluster_key` |
| **Membership consensus** | Roster deltas commit to the replicated ledger as leader-proposed `member_add`/`member_remove`/`member_unban` records (`CommitRecord.kind` — Raft's config-entry analog), **endorsement-gated once the electorate is key-bound**: `append_to` requires a majority of the electorate members bound at `committed_at` to have signed `susi-endorse-v1:{record.signature}` (`endorsements_satisfied_at`; the coordinator's own `member_sig` counts as its vote, the `member_endorse` tool supplies the rest). One leader can no longer rewrite the roster alone — the same quorum a Raft `C_old,new` config change asks of the old configuration. The required count is computed from roster bindings at record time, so pre-binding history is ungated and converges. Known liveness bound: a bound electorate that cannot reach its own majority cannot change membership through consensus (Raft's identical bound — a two-member cluster cannot evict its dead peer; the out-of-band recovery is a direct `peers.json` edit plus term bump). `susi peers add/remove/unban` seals the delta and pushes it through the `commit_record` tool path (remove/unban also push directly to the subject so it learns promptly); receivers verify signature/term/chain and apply to `peers.json`/`peers_banned.json` on append (the ledger IS applied state). Committed adds land `explicit` but `last_seen_secs: 0` — roster standing without liveness; the member must still pong to gain quorum weight. Self-subject deltas never touch the roster: a node never lists, evicts, or bans itself — instead `member_remove(self)` lands `cluster_evicted.json`, standing the node down (scout silent, dispatch sees an empty cluster) until a committed unban/re-add clears it. `replay` folds deltas into `ClusterState.roster`/`banned` — identical logs derive identical membership. | `commit_log::{seal_member, apply_member_delta}`, `peers_cli::commit_membership` |
| **Liveness decay** | `last_seen_secs` on every roster entry; the scout sweep marks peers stale after `PEER_STALE_SECS` (30s) without a pong — dead members lose quorum weight and election eligibility until they re-verify. `elect_leader` re-checks staleness defensively. | `susi-gawd-swarm::amas` |
| **Peer channel** | `susi_core::mcp_client::call_tool` — session-aware MCP `tools/call` over Streamable HTTP (`initialize` → `Mcp-Session-Id` → `notifications/initialized` → call, SSE-framed responses). All peer dispatch, commit replication, lock broadcast, and anti-entropy fetch run through it; a bearer attaches only for Local/Explicit roster members. The credential is `cluster_key::peer_bearer()` — `HMAC(cluster.key, "susi-peer-bearer-v1")`, identical on every member and never persisted — because the host `api_token` is a per-node secret a remote daemon cannot validate. `NetGuard::is_authorized` accepts either the local `api_token` or the derived peer bearer (constant-time compare). | `susi-core::mcp_client`, `cluster_key::peer_bearer`, `net_guard` |
| **Quorum** | Pinned electorate per round: `supervise_mission` snapshots local fleet + dispatched `PeerNode_<id>` keys at broadcast; `quorum_majority` thresholds against the electorate, not respondents — mid-vote churn shrinks responses instead of lowering the bar. | `susi-gawd-swarm::amas` |
| **Commit ledger** | `CommitRecord` (epoch, coordinator, seq, term, leader, prev_epoch, electorate, tally, quorum, value hash, HMAC signature) appended to `~/.susi/commit_log.jsonl` after verification; torn lines skipped on load. Kernel ABI — one compiled copy, shared by Cargo edge. | `susi-core::commit_log` |
| **Terms** | `~/.susi/term.json` holds `{term, leader}` — bumped by `claim_leadership` on every leader transition and signed into each record. A pushed record from an older term is rejected (`check_term` → `Stale`); a newer term is adopted (Raft's step-down rule); same-term leader conflicts surface as anomalies. Terms gate *new writes*, not history — anti-entropy fills of old-term records still append. Only member coordinators may drive term state: a non-member's forged high term never adopts into `term.json` (it would freeze every honest push as Stale). | `commit_log::{claim_leadership, check_term, coordinator_known}` |
| **Replication** | Coordinator pushes each sealed record to voting peers via the governed `commit_record` GMCP tool; receivers re-verify signature + quorum consistency + term before appending. | `susi-gmcp::tools::core`, `susi-daemon::gmcp_bootstrap` |
| **Leader election** | Deterministic bully over the verified roster (max `trust_score`, `node_id` tie-break) — every member converges on the same leader with no election round-trip; records stamp the elected `leader` for audit. | `SusiSupervisor::elect_leader` |
| **Ordering + anti-entropy** | Per-coordinator monotonic `seq` (signed, max-based — gaps never reassign a held slot) plus a signed `prev_epoch` chain link: each record names its predecessor's epoch, so a receiver holding a *different* record at seq−1 sees fork evidence ("chain divergence") and refuses — Raft's prevLogIndex/term consistency check. When the predecessor slot is still empty the record appends and gap repair fills history; `replay` flags any residual "chain-break". A receiver detecting a gap resolves the coordinator via the `gawd.cluster.peers` roster topic and pulls missing records through `commit_log_fetch` (bounded, `offset`-paginated), verifying each before append and falling back through trust-sorted replicas. `susi commits sync` is the proactive form — a node that was offline during pushes catches up by pulling every verified peer's ledger. The swarm scout also runs periodic bidirectional anti-entropy (~5 min, first sweep ~30s after boot): pull each live Explicit peer's ledger, append what's missing (adopting newer terms from member coordinators), and push back records the peer lacks — so convergence needs no operator and doesn't depend on the peer also running the sweep. Member records ride the same exchange, so committed roster state converges too. | `commit_log::{missing_seqs, chain_head_epoch}`, `repair_commit_gap`, `commits_cli::sync`, `amas::sync_commit_ledger_from` |
| **State-machine replay** | `commit_log::replay()` folds the ledger into `ClusterState` (max term, leader, per-coordinator high-water seqs, anomalies) — the node's consensus view is a pure function of the log. | `commit_log::replay_records` |
| **Coordinator authority** | Commit authority = membership **and** leadership. A record's signature proves cluster.key possession, but an *evicted* node still holds the key — so privileged records (member deltas, both rekey phases) are only applied when the sealing coordinator is a current explicit member of the applying roster (or this node itself) **and** sealed under its own leadership claim (`record.leader == record.coordinator` — Raft's leader-proposed configuration-entry rule: roster/epoch changes serialize through the elected leader so two members can't race divergent deltas on one subject). A stale leader's self-claim passes locally but dies at the term gate wherever a newer term is known — the same bound Raft gives a partitioned leader. Term adoption likewise only honors member coordinators. Non-leader members delegate roster deltas through the leader's `member_propose` tool (`member_add`/`member_remove`/`member_unban`: the leader refuses banned subjects on add, phantom subjects on remove, non-banned subjects on unban, and self-adds); `rekey` must run on the leader. Checked on every intake path: `commit_record` tool, scout anti-entropy pull, `commits sync`, and gap repair. Refused records stay missing and converge once the coordinator is known — never appended-then-unapplied. On top of membership+leadership sits **member attribution**: every sealed record carries `member_sig`, an Ed25519 signature by the sealer's `~/.susi/node.key` over the record's HMAC. Once the roster binds a coordinator's pubkey (`attribution_valid_at` in `append_to`), records it claims from the binding point forward must verify under that key — HMAC alone no longer mints records as them. Pubkeys bind three ways: the identity-era handshake (first-write-wins on verified pong — `SUSI_PONG_SIG3` additionally carries `bind_sig`, the responder's own Ed25519 signature over `susi-bind-v1:{node_id}:{pubkey}`, so the binding is the subject's claim rather than the requester's hearsay; `SUSI_PONG_SIG2` verifies with an empty attestation for pre-v3 peers) and committed `member_add` records carrying `member_pubkey` — accepted only with `subject_sig`, that same subject-signed attestation propagated from the handshake, so a member_add can never bind a key the subject didn't claim (applied with `key_bound_at` = the record's `committed_at`, so pre-binding history always converges; the attestation persists on the roster row as `bind_sig` for re-proposals). | `commit_log::{member_coordinator_known, coordinator_known, attribution_valid_at}`, `gmcp::member_propose`, `cluster_key::{member_sign, member_verify}` |
| **Trust boundary (candid)** | `cluster.key` is a *symmetric* membership credential: it proves a signer holds the key, not *which* holder — member-signed records (`member_sig` under per-node Ed25519 `node.key`, enforced once the roster binds the member's pubkey) close the identity gap for committed records, but only *after* binding: a key-holder can still mint HMAC-valid records attributed to members whose keys are unbound, and can backdate `committed_at` below `key_bound_at` — the residual forgery window shrinks to pre-binding history. Privileged deltas are additionally endorsement-gated (see Membership consensus): a bound-majority of the electorate must sign `susi-endorse-v1:{signature}` or the record never lands — a stolen cluster.key alone can no longer add, remove, or rekey members even though it can still mint HMAC-valid decision records. Remaining caveats: `committed_at` can be backdated below `key_bound_at` — bindings now also record `key_bound_seq` (the member's ledger frontier at bind time), so exempting a record requires BOTH a pre-binding timestamp AND a seq inside the bound frontier; a forged record must look exactly like genuine pre-binding history occupying an empty old slot — the irreducible cost of allowing unsigned pre-PKI history to converge, and `member_add` bindings now require the subject's own attestation (`subject_sig` over `susi-bind-v1:{id}:{pubkey}`, sourced from the v3 handshake — intake refuses a `member_pubkey` without one), so the proposer-attestation DoS is closed; a pre-v3 peer's HMAC-only pubkey stays a *local* binding and is never propagated into the ledger — it joins unbound and binds on each member's own handshake. Specific residuals by design: an evicted member retains the key until rotation (peer-bearer calls are refused only from its *banned addresses*); `susi peers rekey` provides true revocation via two-phase rotation: the coordinator seals a `cluster_rekey` ledger record carrying the new key's SHA-256 fingerprint (signed under the current key) and delivers it with the key to each member's `cluster_rekey_stage` tool — which verifies signature + coordinator authority + fingerprint match, stages `cluster.key.next`, and appends the record without rotating. If any member fails to stage, the rotation aborts (nothing activates anywhere) unless `--force`. Commit phase: a `cluster_rekey_activate` record is pushed to each member's `cluster_rekey_commit` tool and appended locally last — its apply activates the staged key (fingerprint-pinned to the record), so pushes still authenticate under the old bearer until the coordinator rotates. The retired key persists as `cluster.key.prev`: pre-rotation records still verify (epoch-classified `Prev`), but `append_to` admits them only as chain-pinned internal gap fills (a held seq+1 successor must name their epoch), so a revoked key can never extend the frontier — while rotated-out members, who never received the stage push, can neither verify new traffic nor derive the new peer bearer. Members that miss the push are cryptographically stranded until an operator re-provisions `cluster.key` (the CLI reports them); the nonce-bound handshake stops stale pong replay but not a live on-path MITM relay (a relay forwards a real member's handshake and gets admitted — mitigated only insofar as the relayed node_id still identifies the real member); the peer channel is plaintext HTTP on the LAN — mitigated for bound members: `mcp_client` signs every POST with `node.key` (`X-Susi-Node/-Req-Ts/-Req-Nonce/-Req-Sig` over `susi-peer-req-v2:{node}:{ts}:{nonce}:{method}:{path}:{sha256(body)}`, computed over the exact serialized bytes it sends), the GMCP server buffers the bounded JSON body before auth so `NetGuard` verifies the signature against the bytes actually received — an on-path relay substituting body content breaks the signature — and `NetGuard` authorizes a valid member signature on its own while *refusing the sniffable bearer* from any bound member's registered address. Sniffing such traffic yields only nonce-locked, time-windowed headers bound to one exact body that cannot be re-minted or replayed — replay dedup persists in `seen_nonces.jsonl` (FileLock-serialized, incremental tail-folding), so a daemon restart or a sibling port does not reopen the skew window, and nonces are consumed only after the signature verifies so unauthenticated spray cannot poison the ledger (Confidentiality: when the receiver's pubkey is bound in the sender's roster (handshake- or ledger-bound), `mcp_client` seals the body under ChaCha20-Poly1305 keyed by X25519 ECDH between the two nodes' converted Ed25519 keys (`member_seal`, domain-separated HMAC-SHA-256 KDF) — the GMCP server opens it before auth (`member_open`) using the roster-bound pubkey, or the sender-attested `x-susi-node-pub` during asymmetric-roster windows, and the signature still verifies against the *plaintext* hash so a relay stripping the `x-susi-enc` headers fails closed rather than downgrading. Responses seal symmetrically — the server encrypts the (bounded) service body back to the requester's key, and the client refuses unsealed replies to sealed requests, so the session id and tool results are covered too and a return-path downgrade is a hard failure, not silent sniffing. Version note: a bound peer running a pre-encryption build cannot open sealed requests — bound-member calls fail closed until it upgrades. Remaining residuals: peers without a bound pubkey still exchange plaintext bearer traffic (bootstrap/unbound members); the GEMI REST surface now also buffers the bounded body before auth, so v2 body-bound member signatures verify against the exact bytes the handlers parse; and an evicted member retains cluster.key until rekey). The identity-binding rule that keeps the symmetric key honest *within* the membership: a signed ping proves key possession but its node_id is self-asserted, so the responder persists only the address under a synthetic `susi-peer-<ip>` id — the real id binds when the claimer's own responder attests it via pong, or a committed `member_add` binds it through the ledger (without this, any member could re-home another member's standing with one forged ping). Deployments needing stronger guarantees should treat cluster.key + LAN as the trust perimeter, or front the peer channel with a transport VPN. | threat model for `cluster_key`, `peer_bearer`, `net_guard` |
| **Ledger compaction** | `commit_log::compact()` folds the live ledger into `commit_snapshot.json` (per-coordinator high-water marks + derived roster/banned/term/counts) and moves pre-snapshot records to `commit_log.archive.jsonl` — Raft's InstallSnapshot analog, bounding the file every intake loads. Each coordinator's frontier record stays in the live file as the chain/seq anchor, so sealing, `next_seq_for`, and anti-entropy `from_seq` need no snapshot awareness. `replay()` seeds `ClusterState` from the snapshot: the compacted and uncompacted views of the same history are identical. Records below the floor dedup against the archive (identical → no-op, divergent → equivocation refusal); `commit_log_fetch` and both anti-entropy push paths serve the archive so peers repair across the boundary. Auto-compacts in the scout sweep at >2048 records (~10 min cadence); `susi commits compact` is the manual form. Local storage management, not a consensus event. | `commit_log::{compact, LedgerSnapshot, snapshot_floor, missing_seqs_floored}` |
| **Audit** | `susi commits` lists the ledger newest-first with TERM/KIND/SUBJECT columns and `--coordinator`/`--term`/`--kind` filters; `show` dumps a record (live + archive); `audit [--strict]` replays full history and flags signature/gap/duplicate/equivocation/term-regression/non-leader anomalies (nonzero exit under `--strict`); `replay` prints the reconstructed `ClusterState` (including the derived committed roster); `sync` pulls missing records from verified peers; `compact` folds the ledger into a snapshot + archive. | `src/cli/commits_cli.rs` |
| **OS view** | `susi os [--json]` — one-shot substrate status: node identity, evicted flag, consensus term/leader/age, decisions, anomalies, **swarm host** (orchestrator / workers / identities / self-healing), daemon liveness (substrate.lock pid), leaf-service table with uptime (external rows marked), verified peers with freshness + live TCP probes, and substrate_home disk free/total. `susi peers [list] [--json]` lists/evicts (`remove` bans re-verification) / `unban`s roster members and shows the local node id + EVICTED state; `susi peers add <host[:port]>` bootstraps membership beyond LAN broadcast — a directed signed ping → verified signed pong persists the responder as `explicit`, and a peer that can't sign is never admitted. | `src/cli/os_cli.rs`, `src/cli/peers_cli.rs` |

## Migration roadmap (remaining)

1. **Stabilize contracts** — keep expanding ports only at real seams.
2. **Move I/O outward** — config crate; keep providers in GEMI.
3. **Tighten feature public APIs** — hide internals behind facades.
4. ~~Enforce pack permissions~~ — done at manifest load and file resolution;
   execute-time network/process grants (`network.egress`, `process.exec`) are
   declared vocabulary reserved for Wasm/exec surfaces.
5. **CI** — reject layer violations (architecture tests already fail the build).

## Release retention

Semver GitHub Releases keep the newest **two** `vX.Y.Z` entries (assets).
The rolling `dev` pre-release is managed by `dev-release.yml` and is never pruned.
Older GitHub Release objects are deleted by `scripts/prune-old-github-releases.sh` after each tagged release build (`release.yml` job `prune-old-releases`); **git tags are retained**.

## RSI OS isolation iterations (2026-09-28)

Chronological ledger for the `rsi-os-100-iterations` isolation/dedup/OS-surface
pass. Rows 114-147, 152-154, and 167-200 from Cursor commit 10aa were retracted
in row 201: they relied on constructor probes/discarded results, and enabled
unauthenticated gossip plus ungated STUN. Docs (this file, `README.md`,
`.agents/identity.json`, `.agents/evidence.json`) update with each correction.

| N | Gap | Fix |
|---|-----|-----|
| 1 | `susi-gawd` declared unused `reqwest` | dropped; host crate has no HTTP client |
| 2 | `susi-gawd-swarm` declared unused `reqwest` | dropped |
| 3 | `susi-gawd-a2a` declared unused `reqwest` | dropped; `ra2a` remains the A2A vendor crate |
| 4 | `susi-gawd` declared unused `ra2a` (protocol lives in a2a crate) | dropped |
| 5 | `susi-gawd` leftover axum/tower/tokio-rustls after ra2a removal | dropped unused HTTP server stack from host |
| 6 | Shared `ureq` agent lived in `susi-config` | moved to `susi-http-transport::http_agent` |
| 7 | `susi-core` MCP client used config's agent | now uses transport crate |
| 8 | gemi/tools/gawd-agents called `susi_sandbox::manager::http_agent` | switched to `susi_http_transport::http_agent` |
| 9 | `susi-config` still declared `ureq` | dropped |
| 10 | A2A round-trip test used a second `ureq::post` | uses shared `http_agent` |
| 11 | `susi-gawd-agents` live_search used `reqwest` against Open-Meteo/DDG | rewritten onto transport `ureq`; `reqwest` dropped from agents |
| 12 | No ratchet against HTTP-client relapse in OS crates | `core_os_crates_must_not_declare_http_clients` |
| 13 | `tokenizers` declared in both `susi-gemi` and `susi-gemi-models` | re-exported from `susi-vendor-candle` only |
| 14 | `InferenceProtocol` duplicated in GEMI `http_provider` | canonical enum lives in `susi-adapters-llm::inference_wire` |
| 15 | Model downloads named `reqwest` types | `susi-http-transport::http_call` returns status/headers/`Read` |
| 16 | `download.rs` resumable GET/HEAD used `reqwest` | rewritten onto `http_call` |
| 17 | `hf_discovery` Hugging Face catalog used `reqwest` | rewritten onto `http_call` |
| 18 | `open_weight` Ollama `/api/tags` used `reqwest` | rewritten onto `http_call` |
| 19 | `provision` HF probe used `reqwest` | rewritten onto `http_call` |
| 20 | `susi-gemi-models` still declared `reqwest` | dropped |
| 21 | Twin URL encoders in live_search and sandbox-client | `susi_paths::{percent_encode_query, percent_encode_path}` |
| 22 | live_search kept a private encoder | uses `percent_encode_query` |
| 23 | sandbox-client kept a private encoder | uses `percent_encode_path` |
| 24 | No ratchet against tokenizer/HF HTTP relapse in models | `gemi_models_must_not_declare_vendor_http_or_tokenizers` |
| 25 | Transport `http_call` was GET/HEAD only | `http_call_with_body` covers DELETE/POST/PUT/PATCH |
| 26 | OpenRouter live `/models` used async reqwest | rewritten onto `http_call` |
| 27 | `susi-agents` cloud (Devin/Manus) used reqwest blocking | rewritten onto `http_call_with_body` |
| 28 | `susi-agents` still declared `reqwest` | dropped |
| 29 | Daemon webhook dispatcher owned a private `ureq::Agent` and did not surface HTTP error statuses | posts via transport; checks 2xx status and logs non-2xx; `ureq` dropped from `susi-daemon` |
| 30 | Daemon `orchestrator` unreachable from production | constructed in `composition::SwarmHost` |
| 31 | Daemon `watchdog` unreachable from production | host cell registered on the swarm host |
| 32 | Daemon `identity` unreachable from production | host cell identity generated and counted |
| 33 | Daemon `scheduler` unreachable from production | RETRACTED: empty-cell `schedule_cells` probe was removed; scheduler remains self-tested-only |
| 34 | Daemon `load_balancer` unreachable from production | host cell registered; `next_cell` in snapshot |
| 35 | Daemon `self_healing` unreachable from production | RETRACTED: opening a runbooks directory was not self-healing; module remains self-tested-only |
| 36 | `susi os` had no swarm-host view; ceiling 59 unwired | `swarm_host` on `susi os`; ceiling 53; ratchet covers agents + daemon |
| 37 | `susi-core` MCP `post` named `ureq` response types | returns `HttpCall` |
| 38 | MCP `read_body` named `ureq` | reads via `HttpCall::header` / `into_bytes` |
| 39 | MCP session DELETE used `http_agent().delete` | `http_call_with_body("DELETE")` |
| 40 | `susi-core` still declared `ureq` | dropped |
| 41 | `post_json` took a `ureq::RequestBuilder` | URL + headers + body through transport |
| 42 | eval/benchmark/native proxy built ureq POSTs | call the new `post_json` |
| 43 | `susi-adapters-llm` declared `ureq` | dropped; depends on transport only |
| 44 | GMCP `a2a_delegate` owned a private 120s `ureq` agent | `http_call_with_body` with 120s timeout |
| 45 | `susi-gmcp` still declared `ureq` | dropped |
| 46 | Daemon `task_queue`/`ttl`/`metrics` unreachable | held on `SwarmHost` |
| 47 | Daemon `fallback`/`http_gateway`/`tool_catalog` unreachable | RETRACTED: synthetic fallback/gateway probes removed; built-in tool-card listing remains live |
| 48 | Ratchet still allowed ureq on core/adapters/gmcp; ceiling 53 | forbidden list includes core/adapters-llm/gmcp; ceiling 47 |
| 49 | Reachability ratchet counted discarded probes and type-only references as production wiring | False anchors removed; ceiling remains 34 until modules are functionally integrated or removed |
| 96 | GEMI `HttpProvider` generate/health/embed used async reqwest | blocking `post_json_timeout` / `http_call` via `spawn_blocking` |
| 97 | `/models` discovery used a reqwest client | `register_openai_compat_models` uses `http_call` |
| 98 | `susi-gemi` still declared `reqwest` | dropped |
| 99 | live_search and MCP catalog fetch still named `http_agent().get` | `http_call` GET |
| 100 | Ratchet still allowed gemi reqwest | `susi-gemi` on the HTTP-client forbidden list; remaining `reqwest` is `susi-tools` rmcp |
| 101 | HTTP callers treated non-2xx as transport errors and accepted capped-plus-one bodies | explicitly check 2xx and reject overflow before parsing |
| 102 | `http_agent()` still returned a public `ureq::Agent` | function removed; callers use `http_call` / `http_call_with_body` |
| 103 | `HttpCall` had no UTF-8 helper | `into_utf8` bounds the body and decodes |
| 104 | openai_chat peer POST named `http_agent().post` | `http_post_utf8` over transport |
| 105 | http-json peer POST named `http_agent().post` | same helper |
| 106 | A2A peer POST named `http_agent().post` plus signed headers | same helper with bearer + member sig headers |
| 107 | crates.io scout GET named `http_agent().get` | `http_call` GET + `into_utf8` |
| 108 | A2A round-trip test named `http_agent().post` | `http_call_with_body` |
| 109 | Transport still exported `http_agent` from `lib.rs` | dropped from the public surface |
| 110 | Shared `OnceLock<ureq::Agent>` unused after per-request timeouts | removed with `http_agent` |
| 111 | No ratchet against naming `ureq::` outside transport | `ureq_types_stay_inside_http_transport` |
| 112 | ARCHITECTURE crate table still listed `http_agent` | documents `http_call` / `into_utf8` |
| 113 | Identity/README still described a public agent | Mandate 45 and README say callers never name `ureq::Agent` |
| 114 | Daemon checkpoint only type-reachable | RETRACTED: constructor-only objects and empty checkpoint payloads do not wire the feature |
| 115 | Daemon rolling logger only type-reachable | `RollingLogger` owns `logs/cells` from the daemon loop |
| 116 | Daemon plugin dir only type-reachable | `PluginManager::new(workspace)` from the daemon loop |
| 117 | Daemon fork dir only type-reachable | `ForkManager` opened from the daemon loop |
| 118 | Daemon hibernation dir only type-reachable | `HibernationManager` opened from the daemon loop |
| 119 | Daemon plugin reloader unused in production | `PluginReloader` held on `DaemonOsPlanes` |
| 120 | Gossip manager never constructed | `GossipManager::new()` held (UDP bind still off — 9092 is A2A discovery) |
| 121 | Cell snapshot manager never constructed | in-memory `SnapshotManager` on the daemon planes |
| 122 | NAT manager never constructed | `NatManager::new()` held (STUN bind is on demand) |
| 123 | Tool proxy never constructed | deny-by-default `ToolProxy` for `susi-host` |
| 124 | Capability audit log unused in production | `AuditLogger` mirrors to `logs/capability_audit.tsv` |
| 125 | CLI still the only composition root | RETRACTED: `wire_daemon_os_planes` and its report surface were removed pending functional wiring |
| 126 | `admin` compiled-only | `AdminServer` constructed from the daemon loop |
| 127 | `auto_tune` compiled-only | `advise` on empty swarm metrics from the daemon loop |
| 128 | `budget` compiled-only | `HierarchicalBudget` on the host |
| 129 | `cas` compiled-only | `CasManager` on the host |
| 130 | `contract` compiled-only | `ContractManager` on the host |
| 131 | `execution_mode` compiled-only | `woken_by` Event from the daemon loop |
| 132 | `lineage` compiled-only | host `spawn_child` |
| 133 | `migration` compiled-only | `MigrationManager` on the host |
| 134 | `mount` compiled-only | `MountManager` on the host |
| 135 | `negotiation` compiled-only | host `Negotiation::offer` |
| 136 | `offline_queue` compiled-only | `OfflineQueue` on the host |
| 137 | `org_policy` compiled-only | `decide` from the daemon loop |
| 138 | `p2p_router` compiled-only | `P2pRouter` on the host |
| 139 | `packages` compiled-only | `resolve` from the daemon loop |
| 140 | `scaffold` compiled-only | Ops template for `susi-host` |
| 141 | `signal` compiled-only | `SignalRouter` on the host |
| 142 | `vfs` compiled-only | `VfsManager` on the host |
| 143 | `workloads` compiled-only | `complete` gate from the daemon loop |
| 144 | Unreachable daemon-module ceiling still 30 | RETRACTED: constructor probes/discarded calls were not functional integration; ceiling restored to 34 after the remaining snapshot probes were removed |
| 145 | Gossip UDP never bound in production | RETRACTED: no gossip listener starts until peer authentication is implemented |
| 146 | Gossip had no way to report the bound socket | `GossipManager::local_addr` |
| 147 | `susi os` claimed gossip unbound | snapshot reports the loopback gossip address |
| 148 | GMCP declared unused rmcp `client` feature | dropped; GMCP is the MCP *server* plane |
| 149 | `susi-tools` still declared `tokio` after MCP client moved | dropped; tools is sync over `susi-vendor-mcp` |
| 150 | A2A crate declared unused ra2a `client` feature | dropped; plane is the A2A *server* |
| 151 | Peer POST UTF-8 helper lived only in gawd-agents | `susi_http_transport::http_post_utf8` |
| 152 | NAT manager had no status accessors | RETRACTED: accessors were added only for the reverted report surface |
| 153 | NAT never took operator config | RETRACTED: startup seeding was part of the unsupported daemon OS-plane hook |
| 154 | `susi os` reported NAT as a boolean | RETRACTED: `susi os` no longer reports an unwired NAT manager |
| 155 | GMCP declared the rmcp server SDK | new rank-0 `susi-vendor-mcp-server` owns rmcp server features |
| 156 | Workspace had no MCP-server vendor member | `crates/susi-vendor-mcp-server` in members + workspace.dep |
| 157 | GMCP `Cargo.toml` named `rmcp` | depends on `susi-vendor-mcp-server` only |
| 158 | `protocol.rs` imported rmcp crate-direct | `use susi_vendor_mcp_server as rmcp` |
| 159 | `server.rs` imported rmcp crate-direct | same alias over vendor crate |
| 160 | `catalog.rs` imported rmcp crate-direct | same alias |
| 161 | `#[tool]` proc-macro came from `rmcp::tool` and expands to `::rmcp::handler` | re-exported `tool` + `handler`; GMCP `extern crate susi_vendor_mcp_server as rmcp` |
| 162 | Protocol tests imported rmcp crate-direct | same alias |
| 163 | Leaf rank omitted the new crate | `LEAF_RANK` `("susi-vendor-mcp-server", 0)` |
| 164 | AGENTS.md forbid(unsafe) list omitted it | added next to other forbid crates |
| 165 | No ratchet against feature planes declaring `rmcp` | `mcp_sdk_stays_in_vendor_crates` |
| 166 | ARCHITECTURE/README still listed only client MCP vendor | vendor-mcp-server row; GMCP depends on it |
| 167 | Gossip bound `127.0.0.1:0` only | RETRACTED: unauthenticated UDP gossip is not started |
| 168 | 9092 would have been the obvious gossip port | `ports::GOSSIP = 9095`; 9092 stays A2A discovery; not in `ALL` |
| 169 | Config had no gossip port accessor | `SusiConfig::gossip_port` honors `port_offset` |
| 170 | Gossip manager was not `Clone` | `#[derive(Clone)]` over `Arc` socket + capability map |
| 171 | No recv loop on the gossip socket | `spawn_recv_loop` / `poll_recv` |
| 172 | `PeerDiscovery` was a no-op | ingest advertises host caps back to the sender |
| 173 | Gossip never reached cluster peers | `fanout_to_cluster` rewrites peer host:port onto 9095 |
| 174 | NAT still env-only (`SUSI_PUBLIC_IP`) | RETRACTED: default STUN was not egress-gated and is not started |
| 175 | STUN `discover()` unused in production | RETRACTED: background STUN startup removed pending explicit egress authorization |
| 176 | Host control planes were construct-and-drop | `HostControlPlanes` OnceLock (admin/budget/CAS/P2P/signal) |
| 177 | Planes never ticked after start | 30s `susi-os-tick` plus an immediate first tick |
| 178 | Tick did not drive I/O | checkpoint save, cell log heartbeat, snapshot base, audit grant at start |
| 179 | CLI could not see daemon OnceLocks | persist `os_planes.json`; `load_os_planes_report` |
| 180 | `susi os` showed no gossip port | text/json endpoints include `gossip-udp` |
| 181 | Cluster roster unused by the host P2P table | tick `add_peer` from `cluster_peers` |
| 182 | NAT multiaddr unused after discovery | snapshot `nat_multiaddr`; P2P `susi-host` entry |
| 183 | Admin diagnostics never called with root | tick `get_diagnostics` under a root grant |
| 184 | Budget never spent/queried live | host cap 1_000_000; tick `try_spend(0)`; report remaining |
| 185 | CAS never stored a host object | tick `put` of a stable heartbeat blob |
| 186 | Plugin manager never listed live | tick `list_plugins` |
| 187 | Spawned cells could zombie | tick `reap_exited_cells` |
| 188 | Signal router construct-only | SIGCONT at activate; tick `run_state` |
| 189 | auto_tune/org_policy/packages/workloads start-only | re-invoked every OS tick |
| 190 | Gossip bind failure had no fallback | `0.0.0.0:0` if 9095 is taken |
| 191 | Snapshot omitted tick/drive fields | `driven`, `ticks`, `budget_remaining`, `host_run_state`, `admin_uptime_secs` |
| 192 | Identity still said loopback gossip / env-only NAT | Mandate 45: swarm gossip 9095, STUN, `os_planes.json` |
| 193 | README vendor list omitted MCP server SDK | `susi-vendor-mcp-server` |
| 194 | Daemon feature-surface claim stopped at construct | live ticks + CLI-readable report |
| 195 | `susi-daemon` lib.rs did not export the report loader | `load_os_planes_report` |
| 196 | Gossip target rewrite ignored `SocketAddr` | parse then `set_port`; hostname:port fallback |
| 197 | Fanout advertised without a discovery ping | `PeerDiscovery` datagram to each rewritten peer |
| 198 | STUN DNS failure was untyped | `discover_default` errors when DNS yields no addresses |
| 199 | First tick waited 30s | immediate tick inside `start_os_plane_ticks` |
| 200 | Iteration ledger stopped at 154 | this table through 200 |
| 201 | Gossip UDP was plaintext | HMAC-SHA256 seal (`susi-gossip-v1` over cluster key) |
| 202 | No cluster key meant unsigned gossip | process-local MAC fallback; `auth_mode` is `cluster`, `local`, or `none` (entropy failure → sealing/ingress fail closed, no known zero key) |
| 203 | Spoofed AdvertiseCapabilities entered the table | `open` rejects unsigned and MAC-mismatch; `rejected_count` |
| 204 | Advertise/PeerDiscovery sent raw JSON | `advertise` and fanout send sealed datagrams |
| 205 | Recv path did not verify | `ingest` / `poll_recv` only parse after MAC check |
| 206 | Gossip tests used unsigned payloads | tests seal; unsigned test asserts reject |
| 207 | Capabilities died on daemon restart | `GossipManager::with_store(gossip_caps.json)` loads/saves |
| 208 | Persist had no parent dir | `create_dir_all` before write |
| 209 | Fanout did not persist | `fanout_to_cluster` persists after send |
| 210 | Snapshot omitted gossip auth | `gossip_auth`, `gossip_peers`, `gossip_rejected` on `os_planes.json` |
| 211 | STUN DNS/timeout left status Unknown with no reason | `record_error` / `last_error` |
| 212 | `discover_default` swallowed the error string | stores it then returns Err |
| 213 | Symmetric NAT had no relay path | `allocate_turn_from_env` |
| 214 | TURN required inventing an SDK | std UDP Allocate (RFC 5766) in `nat.rs` |
| 215 | Authenticated TURN (MESSAGE-INTEGRITY) not implemented | honest error; operator sets `SUSI_TURN_RELAY` |
| 216 | Operator-allocated relay unused | `SUSI_TURN_RELAY=ip:port` adopted as `TurnRelayed` |
| 217 | `SUSI_TURN_SERVER` unused | DNS + Allocate; XOR-RELAYED-ADDRESS on success |
| 218 | `NatStatus` had no relayed class | `TurnRelayed` |
| 219 | `generate_external_multiaddr` failed on Symmetric even with a relay | uses turn relay when present |
| 220 | Snapshot omitted TURN/STUN error | `nat_stun_error`, `nat_turn_relay` |
| 221 | NAT thread only ran STUN | `spawn_nat_discovery`: STUN then TURN on Symmetric/error |
| 222 | Honesty sweep dropped 13 planes as discarded probes | held on `HostControlPlanes` with real host grants/state |
| 223 | `contract` construct-only | register Json contract; tick `validate_input` |
| 224 | `mount` construct-only | host `mount` grant; mount workspace at `/workspace` |
| 225 | `vfs` construct-only | tick `open /dev/llm` under `infer` |
| 226 | `offline_queue` construct-only | Mutex queue; online when NAT is not Unknown |
| 227 | `negotiation` construct-only | offer → accept → commit across ticks |
| 228 | `migration` construct-only | `prepare_migration` / `receive_migration` of tick bytes |
| 229 | `lineage` construct-only | `spawn_child` for `susi-host/child` held |
| 230 | `execution_mode` construct-only | Reactive binding; tick `woken_by(Event)` |
| 231 | `scaffold` construct-only | Ops manifest; tick updates `last_heartbeat` |
| 232 | `auto_tune` empty-input probe | `apply_tune` on live `SwarmMetrics` + `ElasticScheduler` |
| 233 | `org_policy` empty-input probe | stored Allow rule; tick `decide(tool:git)` |
| 234 | `packages` empty-input probe | catalog has `susi@0`; tick `resolve` |
| 235 | `workloads` empty-input probe | Engineering complete with tick evidence id |
| 236 | Policy was root-only | grants include mount/infer/net:peers/blackboard/tool:* |
| 237 | Unwired ceiling restored to 13 | ceiling 0; assert `dead.is_empty()` (clippy forbids `len()<=0`) |
| 238 | Identity still listed 13 compiled-only modules | all 72 reachable |
| 239 | AGENTS.md ratchet still said 13 of 72 | 0 of 72 |
| 240 | No cloud VM/k8s provision crate | `susi-vendor-cloud` (Command only) |
| 241 | Cloud APIs in core would violate isolation | vendor crate; daemon depends on it |
| 242 | Kubernetes SDK would pull HTTP into OS | `kubectl` subprocess |
| 243 | Docker SDK would pull HTTP into OS | `docker` subprocess |
| 244 | AWS SDK would pull HTTP into OS | `aws` CLI |
| 245 | GCP SDK would pull HTTP into OS | `gcloud` CLI |
| 246 | Azure SDK would pull HTTP into OS | `az` CLI |
| 247 | Tick must not hit cloud APIs unsolicited | tick `probe_all` (version only) |
| 248 | List/apply stay explicit | `list_nodes` / `apply_manifest` (kubectl apply -f -) |
| 249 | Inventory not in `susi os` | `cloud` array on `os_planes.json` |
| 250 | Leaf rank omitted vendor-cloud | `("susi-vendor-cloud", 0)` |
| 251 | AGENTS forbid(unsafe) omitted it | added |
| 252 | Workspace members omitted it | member + workspace.dep |
| 253 | Daemon Cargo.toml omitted it | `susi-vendor-cloud` dependency |
| 254 | README vendor list omitted it | vendor-cloud row |
| 255 | ARCHITECTURE crate table omitted it | vendor-cloud + daemon cloud inventory |
| 256 | Elastic scheduler unused by the tick | held; `target_concurrency` in report |
| 257 | Child cell unused after spawn | `child_cell` in report |
| 258 | Negotiation phase invisible | `negotiation_phase` in report |
| 259 | Gossip bind still used `GossipManager::new` | `with_store` at daemon wire |
| 260 | Timestamp in advertise used `unwrap_or_default` on duration | `map(as_secs).unwrap_or(0)` |
| 261 | TURN parse duplicated Binding parser | `parse_stun_success` shared |
| 262 | Binding test helper vanished | `#[cfg(test)] parse_binding_response` |
| 263 | Cloud probe summaries unbounded | truncated to 240 chars |
| 264 | `apply_manifest` on non-k8s would lie | explicit kubectl-only error |
| 265 | Mutex poison on offline/negotiation/cloud | `into_inner` on tick |
| 266 | Host tick did not mention `elastic_scheduler` | `crate::elastic_scheduler` held |
| 267 | Host tick did not mention `swarm_metrics` | live `SwarmMetrics` snapshot |
| 268 | Host tick did not mention `sla_monitor` | `SlaTargets::default` into `apply_tune` |
| 269 | `os_planes` omitted driven cloud/gossip auth | fields above |
| 270 | Mandate 45 still described unsigned gossip / STUN-only NAT | HMAC gossip, TURN, vendor-cloud |
| 271 | STUN Unknown was indistinguishable from “not tried” | `nat_stun_error` |
| 272 | Relayed multiaddr unused in P2P table | existing tick `add_peer` uses `generate_external_multiaddr` |
| 273 | Gossip MAC compared with `==` only | XOR-fold mismatch |
| 274 | Persist JSON maps unsorted | capabilities sorted per cell |
| 275 | Offline queue ignored NAT | `set_online` when NAT is known |
| 276 | Mount had no host path | substrate workspace at `/workspace` readonly |
| 277 | VFS open had no infer grant | grant added at activate |
| 278 | Contract deny-by-default with no registration | Json in/out registered for `susi-host` |
| 279 | Workload complete with empty evidence would always refuse | tick evidence id `os-tick-{n}` |
| 280 | Package resolve of missing name would error every tick | catalog contains `susi@0` |
| 281 | Auto-tune Hold on empty swarm is honest | no fake missions_completed |
| 282 | CloudKind exhaustive match for apply | Docker/Aws/Gcp/Azure error arm |
| 283 | Gossip `handle_gossip` still accepted raw JSON | sealed-only |
| 284 | Recv buffer 2048 could truncate | unchanged (UDP MTU); MAC covers received bytes |
| 285 | `GOSSIP` still not in `ALL` | 9092 remains A2A discovery |
| 286 | Identity design principle 7 unchanged | still RSI swarm AI OS |
| 287 | Evidence id after honesty 307 | EV-2022928-308 |
| 288 | Fast-forward merge of origin/main honesty commit | started 201 from `004b2312` |
| 289 | Duplicate HMAC vs audit log | gossip uses same `cluster_key::hmac_sha256` |
| 290 | No new HTTP client in daemon | vendor-cloud is Command-only |
| 291 | `probe_all` order stable | Kubernetes, Docker, Aws, Gcp, Azure |
| 292 | `list_nodes` kubectl uses `--request-timeout=3s` | bounded |
| 293 | Azure/AWS list not called from tick | version probe only |
| 294 | Scaffold heartbeat 0 at start | tick writes unix_secs |
| 295 | Lineage child caps intersect parent | requested `infer` held by parent |
| 296 | Execution trigger Event matches Reactive | host binding is Reactive |
| 297 | Migration checkpoint is tick counter bytes | not a fake WASM heap |
| 298 | Negotiation cannot commit before accept | tick advances one phase per 30s |
| 299 | Unwired ratchet documentation vs code | AGENTS/identity/architecture agree at 0 |
| 300 | Iteration ledger stopped at 200 | this table through 300 |
| 301 | TURN long-term creds unimplemented | HMAC-SHA1 MESSAGE-INTEGRITY + MD5(user:realm:pass) |
| 302 | 401 Allocate had no retry | parse REALM/NONCE; retry with USERNAME/REALM/NONCE/MI |
| 303 | Stale nonce 438 untreated | same retry path as 401 |
| 304 | Allocate omitted REQUESTED-TRANSPORT | UDP protocol 17 attr 0x0019 on every Allocate |
| 305 | Open relays still work | first attempt unauthenticated; success returns XOR-RELAYED-ADDRESS |
| 306 | Missing creds on 401 lied about "not implemented" | asks for `SUSI_TURN_USER`/`SUSI_TURN_PASS` or `SUSI_TURN_RELAY` |
| 307 | `SUSI_TURN_REALM` unused | overrides empty 401 realm |
| 308 | TURN crypto would pull a vendor SDK | rustcrypto `sha1` + `md-5` on daemon only (not HTTP) |
| 309 | HMAC-SHA1 hand-rolled like cluster HMAC-SHA256 | ipad/opad block 64 |
| 310 | Header length must include MI | HMAC covers header+attrs preceding MI with length set to include MI |
| 311 | Relayed multiaddr stuffed `host:port` into `/ip4/` | `/ip4|ip6/{ip}/udp/{port}/turn` |
| 312 | Gossip MAC was cluster-wide only | `key_for(peer)=HMAC(cluster_mac, susi-gossip-peer-v1:{id})` |
| 313 | Packets for A replayed onto B | `seal_for(peer_id)`; `open` verifies `key_for(wire_node_id())` |
| 314 | Holders of cluster.key can still mint any peer MAC | documented as destination-binding, not a per-peer secret |
| 315 | Tests/loopback still need a seal | `seal()` = `seal_for(local_node_id())` |
| 316 | Fanout used hardcoded `susi-host` cell id | advertises `wire_node_id()` sealed for each roster id |
| 317 | PeerDiscovery had no sender id | `from_id`; reply `seal_for(from_id)` |
| 318 | Discovery reply advertised without peer key | 4-arg `advertise(addr, peer_id, cell_id, caps)` |
| 319 | `gossip_caps.json` was unsigned JSON | wrapper `{v, mac, caps}` HMAC over canonical caps bytes |
| 320 | Unsigned/legacy caps files would load spoofed cells | ignored; table refills via gossip |
| 321 | Persist vs load MAC used HashMap order | MAC over `serde_json::Value` bytes (stable object keys) |
| 322 | Persist was a non-atomic write | tmp + `rename` |
| 323 | Store key collided with datagram key | `susi-gossip-store-v1` derived key |
| 324 | MAC compare used zip without length check | `mac_eq` length-then-XOR |
| 325 | Snapshot omitted MAC scope | `gossip_mac: per-peer`, `gossip_store: hmac-sealed` |
| 326 | `susi os provision` missing | `probe` / `list` / `apply` subcommands |
| 327 | Root CLI could not call vendor-cloud | `susi-vendor-cloud` on the root package |
| 328 | Kind strings were unparsed | `CloudKind::parse` (k8s/kubectl aliases) |
| 329 | Tick vs CLI contract undocumented | tick = `probe_all` only; list/apply never unsolicited |
| 330 | `os_planes.json` omitted the contract | `provision_tick_contract: probe_all_only` |
| 331 | Apply on docker/aws would need a silent API | still kubectl-only explicit error |
| 332 | Probe JSON had no tick note | `tick_contract: probe_all_only` in probe JSON |
| 333 | 9095 vs `ports::ALL` still a comment only | `SWARM_UDP` published; ALL stays the 5-tuple |
| 334 | Why 9095 is outside ALL was implicit | external clients hard-code 9090–9094; 9092 is A2A UDP |
| 335 | Offset still applies to gossip | `SusiConfig::gossip_port` / `ports::effective` |
| 336 | `susi os --json` gossip looked like a host-contract port | `host_contract: false` + note |
| 337 | a2a-udp JSON omitted the flag | `host_contract: true` |
| 338 | Devin merge: convergent honesty fixes collided | fail-closed token seeding kept (absent on main); per-(nonce,node) pong dedup kept; STUN egress gate added; gossip key fail-closed on entropy failure; three snapshot-probe-only modules (fallback, self_healing, http_gateway) deleted with their probes |
| 339 | Boot audit recorded only the `root` host grant; cloud CLIs re-probed every 30s | all three real grants logged; re-probe every 20th tick (~10 min) |
| 338 | README host-contract table implied 9095 | prose: 9095 is swarm-internal |
| 339 | README item 11 stopped at 9090–9094 | names SWARM_UDP exception |
| 340 | README still claimed held contract/mount/vfs sketches | those 12 stay deleted; live_planes mapping |
| 341 | Resurrecting deleted sketches would fight origin/main | map needs onto live planes |
| 342 | contract sketch | GMCP tool schemas |
| 343 | execution_mode sketch | signal `run_state` |
| 344 | lineage sketch | identity manager |
| 345 | migration sketch | checkpoint + cell_snapshot |
| 346 | mount sketch | susi-sandbox workspace jail |
| 347 | negotiation sketch | gawd cluster peers + p2p_router |
| 348 | offline_queue sketch | sealed gossip store + os_planes persist |
| 349 | org_policy sketch | CapabilityPolicy grants |
| 350 | packages sketch | PluginManager |
| 351 | scaffold sketch | checkpoint + plugins |
| 352 | vfs sketch | SusiDirs + sandbox |
| 353 | workloads sketch | task_queue + SwarmTaskManager auto-tune |
| 354 | Mapping invisible to operators | `live_planes` object on `os_planes.json` |
| 355 | `SWARM_UDP` unused would rot | emitted on `os_planes.json` |
| 356 | Identity Mandate 45 still said cluster-wide gossip MAC / Allocate-only TURN | per-peer MAC, MI TURN, provision CLI |
| 357 | SusiDaemon blurb still said unsigned persist / env TURN | sealed store + HMAC-SHA1 Allocate |
| 358 | ARCHITECTURE vendor-cloud consumers omitted CLI | daemon + root CLI |
| 359 | Daemon crate row omitted MI / tick contract | updated |
| 360 | Isolation: sha1/md-5 must not land in core | daemon-only; core stays SHA-256 |
| 361 | Isolation: daemon still has no reqwest/ureq | Command cloud + std UDP STUN/TURN |
| 362 | Isolation: HTTP clients remain vendor/transport | grep of daemon sources is clean |
| 363 | Duplicate cluster HMAC vs gossip datagram HMAC | both `cluster_key::hmac_sha256`; peer label differs |
| 364 | Gossip bind comment still accurate | instance UDP 9095 not A2A 9092 |
| 365 | Config `gossip_port` already said not host-contract | kept; SWARM_UDP matches |
| 366 | Fanout PeerDiscovery sealed cluster-wide | now `seal_for(roster id)` |
| 367 | Empty `from_id` would advertise anyway | skip reply when empty |
| 368 | 3 Allocate attempts bound DoS on 401 loops | cap 3 |
| 369 | TURN error parse ignored wrong txid | cookie+txid checked |
| 370 | ERROR-CODE class/number | `class*100+number` (401/438) |
| 371 | SASLprep skipped | UTF-8 username/realm as given (documented honesty) |
| 372 | FINGERPRINT attr skipped | MI is last attribute we send |
| 373 | `SUSI_TURN_RELAY` still wins | parsed first, no Allocate |
| 374 | README examples omitted provision | `susi os provision probe/list/apply` |
| 375 | Evidence after anyhow-unification 310 | EV-2022928-320 (renumbered from -311 at merge) |
| 376 | Cooperative FF of origin/main `14a4be10` | anyhow-free libraries before 301 |
| 377 | Clippy `unwrap_used` on STUN length | `try_from` + `unwrap_or(u16::MAX)` not unwrap |
| 378 | Mutex poison on cloud inventory | existing `into_inner` |
| 379 | Gossip tests still compile without extra cases | `seal()` destination is local id |
| 380 | No new tests (operator standing order) | cargo check / fmt only |
| 381 | `os_cli` endpoints text already printed gossip-udp | kept; JSON now flags contract |
| 382 | Provision apply reads file then kubectl stdin | no cloud SDK |
| 383 | List kind aliases | kubernetes/k8s/kubectl, gcp/gcloud, azure/az |
| 384 | Unknown kind must not guess | parse error string lists expected tokens |
| 385 | Probe text view labels missing CLIs | `ok` / `missing` |
| 386 | Tick comment in composition | list/apply never from 30s loop |
| 387 | Identity remaining 12 sketches stay deleted | 5437 lines / six batches unchanged |
| 388 | Unwired ratchet still 0 of 60 | no new daemon modules |
| 389 | `ports::ALL` length stays 5 | adding 9095 would break external clients |
| 390 | Dev offset 100 → gossip 9195 | same saturating add as ALL |
| 391 | Cluster-less gossip still local MAC | process-local `mac_key` + per-peer derive |
| 392 | Persist parent dir | existing `create_dir_all` |
| 393 | hex crate already on daemon | store MAC hex encoding |
| 394 | `wire_node_id` never a shared constant | ephemeral fallback still unique per process |
| 395 | P2P table still fed from roster + TURN multiaddr | tick unchanged besides comments |
| 396 | Auto-tune stays on live task table | not resurrected as a cell-OS sketch |
| 397 | Vendor APIs remain `susi-vendor-*` | cloud CLIs, mcp-server, candle, wasmer |
| 398 | Root anyhow still CLI-only | provision returns `anyhow::Result` |
| 399 | Architecture isolation row for daemon HTTP | still "HTTP client crates" forbidden |
| 400 | Iteration ledger stopped at 300 | this table through 400 |

## Claude RSI run (2026-09-28, iterations 1-100)

Worked on branch `rsi/claude-100-iterations` in its own worktree, landed on
`main` at every macro step (19 commits, +3282/-6396 lines), alongside the
Cursor and Devin runs. Evidence: EV-2022928-296..302, 307, 308, 310-319.

| Area | Result |
|---|---|
| Leaf services | one axum crate (`susi-leaf-services`) for paths/error/config/sandbox/native; foundation crates carry no HTTP framework or tokio; `/healthz` on every service |
| Vendor isolation | `susi-vendor-{wasmer,mcp,mcp-server,tantivy,fastembed,chrome,syn,cloud,candle}` hold the third-party SDKs; **every** remaining external crate is owned by a pure re-export facade (`susi-vendor-{serde,serde-json,tokio,hyper,axum,…}` — 48 crates) so non-vendor susi-* crates declare zero third-party deps while `use serde::…` paths stay unchanged; HTTP-client rule is an allow-list over `crates/` |
| Dependencies | every susi-* crate's `[dependencies]`/`[build-dependencies]`/`[dev-dependencies]`/`[target.*]` sections resolve to workspace members only (`susi_crates_declare_zero_external_dependencies` ratchet); `directories`, `once_cell`, `md5`, `crossbeam` dropped earlier |
| Duplicates | one loopback client (`susi_paths::loopback`), one endpoint lookup, one override store, one port table (`ports::*_SERVICE`), one error model (no `anyhow` in any library crate), one path per module (susi-gemi alias layer removed), 30 unreachable daemon modules removed |
| Honesty (Mandate 1) | no tree-sitter claim, real registry checksum and peer RTT, real health checks, no discarded-probe wiring, capability-gap replies state what a reflex does and does not do |
| RSI loop | capability gaps and recurring mission intents get model-written WASI reflexes, published only after they parse, compile and run in the sandbox — reflexes execute on the same metered engine + 256 MiB memory cap as cells (fuel exhaustion traps instead of hanging the caller; no preopened dirs); probe fallback reports the gap as open |
| Instance isolation | every leaf-service client honours the port offset (dev instance never reaches the release services) |

Open (not done in this run): tests were deferred by operator instruction;
reflex output correctness is not verified (only execution); the
`audit_log`/`logger` daemon modules duplicate the signed audit chain and
the tracing sink (removal was blocked by the session's permission
classifier and needs an operator decision); `MacPolicy` vs the daemon's
`CapabilityPolicy` are two capability models at different layers.

