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

## Crate boundaries

| Crate | Responsibility | Public API (shape) | May import | Must not import | Replaceable at runtime | Owns state | I/O |
|-------|----------------|--------------------|------------|-----------------|------------------------|------------|-----|
| `susi-abi` | Universal 0-dependency Swarm OS ABI: AI syscalls, stigmergic pheromones, evidence receipts, wire framing | `SyscallOp`, `SwarmPheromone`, `ToolReceipt`, `WireFrame` | std, serde, serde_json | all workspace crates | no | no | no |
| `susi-paths` | Leaf REST (`:18080`): XDG / substrate paths + host-contract ports | `SusiDirs`, `ports` | std | everything else | no | no | reads env for XDG |
| `susi-error` | Leaf REST (`:18081`): stable error model + metrics sink | `EaiError`, `EaiResult` | std, serde_json | candle, HTTP, feature crates | no | append-only metrics file | yes (metrics) |
| `susi-core` | Domain + ports (kernel ABI): Evidence, Truth, Provider, Tool, CapabilityRegistry, **`plane_bus`** facades + `plane_bus_ipc` rendezvous | traits + ledger types + `plane_bus::{gemi,gawd,tools,agents}` | locally compiled ABI source; vendored paths/error/config IPC; std, serde, concurrency libs | all workspace crates | providers/tools via registry | process registry | receipt archive paths |
| `susi-native` | Leaf REST (`:18084`): Wasmer Wasm host | `WasmHost` | vendored error IPC; wasmer (service only) | feature planes | Wasm modules | instance | yes |
| `susi-config` | Leaf REST (`:18082`): `SusiConfig` + extension packs + versioned JSON store | `SusiConfig`, `extensions`, `VersionedJsonStore` | vendored paths/error IPC; serde, ureq | everything above paths/error | no | config files | yes |
| `susi-sandbox` | Leaf REST (`:18083`): Docker sandbox + daemon integrity (re-exports config via `manager`) | `SandboxManager`, `manager` | vendored paths/error/config IPC; bollard (service only) | gawd/gmcp (prefer hooks) | Docker optional | config files | yes |
| `susi-tools` | Tool registry + MCP client adapters + `plane_handler` | `ToolRegistry`, `EngineHooks`, bus handler | **vendored `susi_core` subset** (`src/susi_core/`): registry/capture/mac_policy over the bus rendezvous; vendored sandbox/config/native/error IPC, rmcp/reqwest | **all workspace crates** (zero-dep consumer); peers via vendored `plane_bus` | tools | registry | yes |
| `susi-agents` | External peer adapters + meta registry (`plane_handler`); domain types live in core | external managers, registry | **vendored `susi_core` subset** (`src/susi_core/`): registry/task_manager/agent_types over the bus rendezvous; vendored sandbox/config IPC | **all workspace crates** (zero-dep consumer); peers via vendored `plane_bus` | peers | registries | yes |
| `susi-gemi-models` | Model select / provision / catalogs | lifecycle, catalogs | **vendored `susi_core` subset** (`src/susi_core/`): task_manager only; vendored sandbox/config IPC | gemi engines crate; peer feature crates | catalogs | cache dirs | yes |
| `susi-gemi` | Inference adapters (Candle, HTTP, MCP-as-provider) | providers, engines, `plane_handler` | locally compiled model-tier and ABI source + **vendored `susi_core` subset**, vendored sandbox/config IPC | **all workspace crates** | providers | model weights | yes |
| `susi-gawd-agents` | Fleet, safety/security, peers | agents, detectors, `plane_handler` topics via agents crate | **vendored `susi_core` subset** (`src/susi_core/`); vendored sandbox/config IPC | **all workspace crates** (zero-dep consumer) | agents | mission-local | yes |
| `susi-gawd-swarm` | AMA / DAG / cloud recovery | swarm dispatch | locally compiled agents-tier source + **vendored `susi_core` subset**, vendored sandbox/config IPC | **all workspace crates** | no | blackboard | yes |
| `susi-gawd-a2a` | A2A (`ra2a`) wire | task store, executor | locally compiled agents-tier source | **all workspace crates** | transport | tasks | yes |
| `susi-gawd` | Host facade: admin, evolution, reflex synth | re-exports + host modules | locally compiled agents/swarm/a2a source + **vendored `susi_core` subset** + vendored native IPC | **all workspace crates** | no | genome/reflexes | yes |
| `susi-gmcp` | MCP HTTP/stdio server + core tools | MCP surfaces, `plane_handler` via tools/agents/gawd bus | **vendored `susi_core` subset** (`src/susi_core/`): plane_bus/intent_bus/agent_tx/mac over the bus rendezvous; vendored sandbox/config IPC, rmcp | **all workspace crates** (zero-dep consumer); swarm/admin via `plane_bus::gawd` / `gawd_hooks` | MCP servers | sessions | yes |
| `susi-server` | Hyper HTTP adapters for GEMI REST | bind helpers | **vendored `susi_core` subset** (`src/susi_core/`): plane_bus facades over `IpcPlaneBus`, file-backed broker, context graph bound to the shared workspace JSONL; vendored sandbox/config/paths/error IPC | **all workspace crates** (zero-dep consumer); GAWD/GEMI via vendored `plane_bus` | no | — | yes |
| `susi-daemon` | Persistent host: lock, ports, composition, rediscovery | `SusiDaemon`, `composition`, `gmcp_bootstrap` | **all** feature crates + server + tools + agents (composition root) | — | no | lock/PID | yes |
| `susi` (root) | CLI + composition entry for workspace intents | `main`, CLI modules | daemon + feature crates + leaf `susi-paths`/`susi-error` (real deps, not vendored) | — | — | cwd workspace | yes |

Workspace crate cycles must remain **zero**. Feature planes have **zero Cargo
peer dependencies** on each other (no `susi-gemi` ↔ `susi-gawd` ↔ `susi-tools`
↔ `susi-agents` ↔ `susi-gmcp` ↔ `susi-server` edges). They communicate only
through **`susi_core::plane_bus`** (topics + JSON DTOs) and shared foundation
(`susi-core`, plus vendored IPC clients for the leaf REST services
`susi-paths` / `susi-error` / `susi-config` / `susi-sandbox` / `susi-native`
on `127.0.0.1:18080–18084`). **`susi-daemon`** and the root **`susi`** package
register `plane_handler` implementations and may link every plane; the root
package may still Cargo-depend on `susi-sandbox` / `susi-native` as
composition-root re-exports.

`susi-core` is the final leaf-service conversion (`127.0.0.1:18085`,
`SUSI_CORE_PORT`, reserved) — the microkernel step. Vendored `susi_core`
copies cannot share `PlaneBus::global()`/`CapabilityRegistry::global()`
statics (each vendored module is a distinct type), so vendored trees back
`plane_bus` with **`plane_bus_ipc::IpcPlaneBus`**: a filesystem
rendezvous under `<cache>/bus/<pid>/` (endpoint files for exact topics and
prefixes) plus a lazily-bound per-copy `127.0.0.1:0` listener serving
`POST /handle` and `POST /stream`. Stream ids embed the opener's endpoint
(`ipc://<addr>/<id>`) so `stream_emit` routes cross-copy; stale endpoint
files are pruned on connect failure and dead pid dirs swept via `/proc`.
Writes stay scoped to the owning process's pid dir (ownership/liveness),
while **reads scan every numeric-named sibling pid dir** — own dir first,
then sorted siblings — so vendored copies *and separate plane processes*
resolve each other's registrations through the same `bus/` root. Exact
topic registrations outrank prefix handlers process-wide.
**`registry_ipc::IpcCapabilityRegistry`**
applies the same pattern to the capability catalog: `register_tool` keeps
the trait object local, serves `capability.tool.<name>` on the owner's bus
(MAC + evidence capture run in the owner-side dispatch handler), and writes
metadata under `caps/`; lookups in other copies return a `RemoteTool` /
`RemoteProvider` proxy that forwards `execute`/`generate`/`embed` over the
bus. Agent capabilities are pure data — `caps/agent/` files only. Process-
global registries (`MacPolicy` — already file-keyed via
`~/.susi/mac.hmac.key`, `IpcBroker`, `IntentBus`, …) become service- or
endpoint-backed the same way when consumers vendor `susi_core`.

**Vendored consumers: `susi-server`, `susi-tools`, `susi-agents`,
`susi-gmcp`, `susi-gawd-agents`, `susi-gawd-swarm`, `susi-gawd`,
`susi-gemi`.** Each carries a
`src/susi_core/` tree (canonical:
`crates/susi-core/vendor_template/susi_core/`) — `plane_bus` facades with
`PlaneBus` delegating to `IpcPlaneBus`, `plane_bus_ipc`, a file-backed
`IpcBroker` under `<cache>/bus/<pid>/broker/` (grants/requests/inbox as
JSON files — the in-crate `susi_core::broker::IpcBroker` is the same
file-backed implementation, so CLI and daemon broker state interop),
`registry` delegating to `IpcCapabilityRegistry` (so tools
registered in one copy dispatch cross-copy and cross-process),
`capture`/`evidence`/
`receipt_archive` with a receipts inbox drained by the owning session
(session rendezvous scans sibling pid dirs),
file-backed `mac_policy` sharing `~/.susi/mac.hmac.key`,
`intent_bus` (providers/needs persisted under the rendezvous, scanned
across pid dirs so cross-process matching works), `agent_tx` (per-copy
`open` map; durable
`.susi/tx/` journal is already workspace-shared), `bus` (per-copy
`TypedEventBus` — the only typed subscribers live in their publishing
crate today), plus
`context_graph`/`net_guard`/`telemetry`/`agent_types`/`provider`/
`task_manager`/`truth`/`manifold`/`queue`/`service_table` (telemetry
history file-backed; the service table is a single shared file under
`substrate_home/services.json`; task handles and pulse queues stay
per-copy) with
`crate::` paths remounted to the vendored tree. `mod.rs` re-exports the
crate-root leaf modules (`susi_core::susi_error`, `susi_core::redact`) so
call sites resolve unchanged. `GemiServer::start_http_server` binds the
vendored `ContextGraph` to `<workspace>/context_graph.jsonl` — the same
log the daemon's copy uses, and every read path already `replay()`s it.
Vendored `#[cfg(test)]` modules run once per consumer test binary — the
same assertions then prove every vendored copy, not just the canonical
crate. All copies must remain byte-identical to the template.

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
service; consumers vendor a byte-identical `susi_config` module (IPC + local
fallback). Vendored `susi_sandbox::manager` still re-exports that surface so
existing import paths keep resolving.

## Architecture tests

`tests/architecture_tests.rs` asserts:

- No workspace crate dependency cycles.
- Forbidden edges (notably `susi-core` / `susi-error` purity and **zero
  peer deps between feature planes** — communication via `plane_bus` only).
- Layer matrix from this document.
- `ARCHITECTURE.md` documents `plane_bus`.

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
| **Autonomous planning loop** | `susi plan`, `SusiMasterAgent::solve_autonomous` | Decompose goal → multi-step mission pipeline → abort on failure → synthesize |
| **Inter-app permission / IPC broker** | `susi broker`, MCP `ipc_*`, HTTP `/broker/*` | Grant / request / negotiate / dispatch messages between identities |
| **Host telemetry** | `susi telemetry`, MCP `host_telemetry`, HTTP `/telemetry`, daemon watchdog | Linux thermal / battery / load; throttles fleet concurrency under stress |
| **Apply-patch-then-test** | `susi patch`, MCP `apply_patch_cycle`, HTTP `/patch/apply`, `SelfHealingAgent` via `AdminHooks` | Workspace-confined edit → test → rollback on failure; gated by `trust_level` / `auto_apply` |
| **MAC + edge privacy** | `susi privacy`, MCP `privacy_*`, `MacPolicy` on every tool | HMAC capability tokens; `local_only` blocks cloud inference + network egress unless consented; mandatory Docker sandbox for host exec |
| **Semantic intent bus** | `susi intent`, MCP `intent_*`, fleet recruitment | NL/hash-embedding provider↔need matching; publishes `IntentRouted` on typed bus |
| **Ambient context sync** | `susi ambient`, daemon ambient indexer | FS mtime poll → ContextGraph + SemanticIndex refresh |
| **Multi-agent transactions** | `susi tx`, MCP `tx_*`, patch/plan loops | File (+ optional blackboard) snapshots with commit/abort restore |

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
| **Commit ledger** | `CommitRecord` (epoch, coordinator, seq, term, leader, prev_epoch, electorate, tally, quorum, value hash, HMAC signature) appended to `~/.susi/commit_log.jsonl` after verification; torn lines skipped on load. Kernel ABI — vendored to all consumers. | `susi-core::commit_log` |
| **Terms** | `~/.susi/term.json` holds `{term, leader}` — bumped by `claim_leadership` on every leader transition and signed into each record. A pushed record from an older term is rejected (`check_term` → `Stale`); a newer term is adopted (Raft's step-down rule); same-term leader conflicts surface as anomalies. Terms gate *new writes*, not history — anti-entropy fills of old-term records still append. Only member coordinators may drive term state: a non-member's forged high term never adopts into `term.json` (it would freeze every honest push as Stale). | `commit_log::{claim_leadership, check_term, coordinator_known}` |
| **Replication** | Coordinator pushes each sealed record to voting peers via the governed `commit_record` GMCP tool; receivers re-verify signature + quorum consistency + term before appending. | `susi-gmcp::tools::core`, `susi-daemon::gmcp_bootstrap` |
| **Leader election** | Deterministic bully over the verified roster (max `trust_score`, `node_id` tie-break) — every member converges on the same leader with no election round-trip; records stamp the elected `leader` for audit. | `SusiSupervisor::elect_leader` |
| **Ordering + anti-entropy** | Per-coordinator monotonic `seq` (signed, max-based — gaps never reassign a held slot) plus a signed `prev_epoch` chain link: each record names its predecessor's epoch, so a receiver holding a *different* record at seq−1 sees fork evidence ("chain divergence") and refuses — Raft's prevLogIndex/term consistency check. When the predecessor slot is still empty the record appends and gap repair fills history; `replay` flags any residual "chain-break". A receiver detecting a gap resolves the coordinator via the `gawd.cluster.peers` roster topic and pulls missing records through `commit_log_fetch` (bounded, `offset`-paginated), verifying each before append and falling back through trust-sorted replicas. `susi commits sync` is the proactive form — a node that was offline during pushes catches up by pulling every verified peer's ledger. The swarm scout also runs periodic bidirectional anti-entropy (~5 min, first sweep ~30s after boot): pull each live Explicit peer's ledger, append what's missing (adopting newer terms from member coordinators), and push back records the peer lacks — so convergence needs no operator and doesn't depend on the peer also running the sweep. Member records ride the same exchange, so committed roster state converges too. | `commit_log::{missing_seqs, chain_head_epoch}`, `repair_commit_gap`, `commits_cli::sync`, `amas::sync_commit_ledger_from` |
| **State-machine replay** | `commit_log::replay()` folds the ledger into `ClusterState` (max term, leader, per-coordinator high-water seqs, anomalies) — the node's consensus view is a pure function of the log. | `commit_log::replay_records` |
| **Coordinator authority** | Commit authority = membership **and** leadership. A record's signature proves cluster.key possession, but an *evicted* node still holds the key — so privileged records (member deltas, both rekey phases) are only applied when the sealing coordinator is a current explicit member of the applying roster (or this node itself) **and** sealed under its own leadership claim (`record.leader == record.coordinator` — Raft's leader-proposed configuration-entry rule: roster/epoch changes serialize through the elected leader so two members can't race divergent deltas on one subject). A stale leader's self-claim passes locally but dies at the term gate wherever a newer term is known — the same bound Raft gives a partitioned leader. Term adoption likewise only honors member coordinators. Non-leader members delegate roster deltas through the leader's `member_propose` tool (`member_add`/`member_remove`/`member_unban`: the leader refuses banned subjects on add, phantom subjects on remove, non-banned subjects on unban, and self-adds); `rekey` must run on the leader. Checked on every intake path: `commit_record` tool, scout anti-entropy pull, `commits sync`, and gap repair. Refused records stay missing and converge once the coordinator is known — never appended-then-unapplied. On top of membership+leadership sits **member attribution**: every sealed record carries `member_sig`, an Ed25519 signature by the sealer's `~/.susi/node.key` over the record's HMAC. Once the roster binds a coordinator's pubkey (`attribution_valid_at` in `append_to`), records it claims from the binding point forward must verify under that key — HMAC alone no longer mints records as them. Pubkeys bind three ways: the identity-era handshake (first-write-wins on verified pong — `SUSI_PONG_SIG3` additionally carries `bind_sig`, the responder's own Ed25519 signature over `susi-bind-v1:{node_id}:{pubkey}`, so the binding is the subject's claim rather than the requester's hearsay; `SUSI_PONG_SIG2` verifies with an empty attestation for pre-v3 peers) and committed `member_add` records carrying `member_pubkey` — accepted only with `subject_sig`, that same subject-signed attestation propagated from the handshake, so a member_add can never bind a key the subject didn't claim (applied with `key_bound_at` = the record's `committed_at`, so pre-binding history always converges; the attestation persists on the roster row as `bind_sig` for re-proposals). | `commit_log::{member_coordinator_known, coordinator_known, attribution_valid_at}`, `gmcp::member_propose`, `cluster_key::{member_sign, member_verify}` |
| **Trust boundary (candid)** | `cluster.key` is a *symmetric* membership credential: it proves a signer holds the key, not *which* holder — member-signed records (`member_sig` under per-node Ed25519 `node.key`, enforced once the roster binds the member's pubkey) close the identity gap for committed records, but only *after* binding: a key-holder can still mint HMAC-valid records attributed to members whose keys are unbound, and can backdate `committed_at` below `key_bound_at` — the residual forgery window shrinks to pre-binding history. Privileged deltas are additionally endorsement-gated (see Membership consensus): a bound-majority of the electorate must sign `susi-endorse-v1:{signature}` or the record never lands — a stolen cluster.key alone can no longer add, remove, or rekey members even though it can still mint HMAC-valid decision records. Remaining caveats: `committed_at` can be backdated below `key_bound_at` — bindings now also record `key_bound_seq` (the member's ledger frontier at bind time), so exempting a record requires BOTH a pre-binding timestamp AND a seq inside the bound frontier; a forged record must look exactly like genuine pre-binding history occupying an empty old slot — the irreducible cost of allowing unsigned pre-PKI history to converge, and `member_add` bindings now require the subject's own attestation (`subject_sig` over `susi-bind-v1:{id}:{pubkey}`, sourced from the v3 handshake — intake refuses a `member_pubkey` without one), so the proposer-attestation DoS is closed; a pre-v3 peer's HMAC-only pubkey stays a *local* binding and is never propagated into the ledger — it joins unbound and binds on each member's own handshake. Specific residuals by design: an evicted member retains the key until rotation (peer-bearer calls are refused only from its *banned addresses*); `susi peers rekey` provides true revocation via two-phase rotation: the coordinator seals a `cluster_rekey` ledger record carrying the new key's SHA-256 fingerprint (signed under the current key) and delivers it with the key to each member's `cluster_rekey_stage` tool — which verifies signature + coordinator authority + fingerprint match, stages `cluster.key.next`, and appends the record without rotating. If any member fails to stage, the rotation aborts (nothing activates anywhere) unless `--force`. Commit phase: a `cluster_rekey_activate` record is pushed to each member's `cluster_rekey_commit` tool and appended locally last — its apply activates the staged key (fingerprint-pinned to the record), so pushes still authenticate under the old bearer until the coordinator rotates. The retired key persists as `cluster.key.prev`: pre-rotation records still verify (epoch-classified `Prev`), but `append_to` admits them only as chain-pinned internal gap fills (a held seq+1 successor must name their epoch), so a revoked key can never extend the frontier — while rotated-out members, who never received the stage push, can neither verify new traffic nor derive the new peer bearer. Members that miss the push are cryptographically stranded until an operator re-provisions `cluster.key` (the CLI reports them); the nonce-bound handshake stops stale pong replay but not a live on-path MITM relay (a relay forwards a real member's handshake and gets admitted — mitigated only insofar as the relayed node_id still identifies the real member); the peer channel is plaintext HTTP on the LAN — mitigated for bound members: `mcp_client` signs every POST with `node.key` (`X-Susi-Node/-Req-Ts/-Req-Nonce/-Req-Sig` over `susi-peer-req-v2:{node}:{ts}:{nonce}:{method}:{path}:{sha256(body)}`, computed over the exact serialized bytes it sends), the GMCP server buffers the bounded JSON body before auth so `NetGuard` verifies the signature against the bytes actually received — an on-path relay substituting body content breaks the signature — and `NetGuard` authorizes a valid member signature on its own while *refusing the sniffable bearer* from any bound member's registered address. Sniffing such traffic yields only nonce-locked, time-windowed headers bound to one exact body that cannot be re-minted or replayed — replay dedup persists in `seen_nonces.jsonl` (FileLock-serialized, incremental tail-folding), so a daemon restart or a sibling port does not reopen the skew window, and nonces are consumed only after the signature verifies so unauthenticated spray cannot poison the ledger (Confidentiality: when the receiver's pubkey is bound in the sender's roster (handshake- or ledger-bound), `mcp_client` seals the body under ChaCha20-Poly1305 keyed by X25519 ECDH between the two nodes' converted Ed25519 keys (`member_seal`, domain-separated HMAC-SHA-256 KDF) — the GMCP server opens it before auth (`member_open`) using the roster-bound pubkey, or the sender-attested `x-susi-node-pub` during asymmetric-roster windows, and the signature still verifies against the *plaintext* hash so a relay stripping the `x-susi-enc` headers fails closed rather than downgrading. Responses seal symmetrically — the server encrypts the (bounded) service body back to the requester's key, and the client refuses unsealed replies to sealed requests, so the session id and tool results are covered too and a return-path downgrade is a hard failure, not silent sniffing. Version note: a bound peer running a pre-encryption build cannot open sealed requests — bound-member calls fail closed until it upgrades. Remaining residuals: peers without a bound pubkey still exchange plaintext bearer traffic (bootstrap/unbound members); the GEMI REST surface now also buffers the bounded body before auth, so v2 body-bound member signatures verify against the exact bytes the handlers parse; and an evicted member retains cluster.key until rekey). The identity-binding rule that keeps the symmetric key honest *within* the membership: a signed ping proves key possession but its node_id is self-asserted, so the responder persists only the address under a synthetic `susi-peer-<ip>` id — the real id binds when the claimer's own responder attests it via pong, or a committed `member_add` binds it through the ledger (without this, any member could re-home another member's standing with one forged ping). Deployments needing stronger guarantees should treat cluster.key + LAN as the trust perimeter, or front the peer channel with a transport VPN. | threat model for `cluster_key`, `peer_bearer`, `net_guard` |
| **Ledger compaction** | `commit_log::compact()` folds the live ledger into `commit_snapshot.json` (per-coordinator high-water marks + derived roster/banned/term/counts) and moves pre-snapshot records to `commit_log.archive.jsonl` — Raft's InstallSnapshot analog, bounding the file every intake loads. Each coordinator's frontier record stays in the live file as the chain/seq anchor, so sealing, `next_seq_for`, and anti-entropy `from_seq` need no snapshot awareness. `replay()` seeds `ClusterState` from the snapshot: the compacted and uncompacted views of the same history are identical. Records below the floor dedup against the archive (identical → no-op, divergent → equivocation refusal); `commit_log_fetch` and both anti-entropy push paths serve the archive so peers repair across the boundary. Auto-compacts in the scout sweep at >2048 records (~10 min cadence); `susi commits compact` is the manual form. Local storage management, not a consensus event. | `commit_log::{compact, LedgerSnapshot, snapshot_floor, missing_seqs_floored}` |
| **Audit** | `susi commits` lists the ledger newest-first with TERM/KIND/SUBJECT columns and `--coordinator`/`--term`/`--kind` filters; `show` dumps a record (live + archive); `audit [--strict]` replays full history and flags signature/gap/duplicate/equivocation/term-regression/non-leader anomalies (nonzero exit under `--strict`); `replay` prints the reconstructed `ClusterState` (including the derived committed roster); `sync` pulls missing records from verified peers; `compact` folds the ledger into a snapshot + archive. | `src/cli/commits_cli.rs` |
| **OS view** | `susi os [--json]` — one-shot substrate status: node identity, evicted flag, consensus term/leader/age, decisions, anomalies, daemon liveness (substrate.lock pid), leaf-service table with uptime (external rows marked), verified peers with freshness + live TCP probes, and substrate_home disk free/total. `susi peers [list] [--json]` lists/evicts (`remove` bans re-verification) / `unban`s roster members and shows the local node id + EVICTED state; `susi peers add <host[:port]>` bootstraps membership beyond LAN broadcast — a directed signed ping → verified signed pong persists the responder as `explicit`, and a peer that can't sign is never admitted. | `src/cli/os_cli.rs`, `src/cli/peers_cli.rs` |

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
