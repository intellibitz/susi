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
        ↓
susi-core (Evidence, Truth, Provider, Tool, CapabilityRegistry)
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
| `susi-paths` | XDG / substrate paths + host-contract port constants | `SusiDirs`, `ports` | std | everything else | no | no | reads env for XDG |
| `susi-error` | Stable error model | `EaiError`, `EaiResult` | std, serde_json, `susi-paths` | candle, HTTP, feature crates | no | append-only metrics file | yes (metrics) |
| `susi-core` | Domain + ports: Evidence, Truth, Provider, Tool, CapabilityRegistry, bus | traits + ledger types | `susi-error`, std, serde, concurrency libs | gemi/gmcp/gawd/sandbox/native/daemon/server/reqwest/hyper/candle/wasmer/bollard/rmcp | providers/tools via registry | process registry | receipt archive paths |
| `susi-native` | Wasmer Wasm host | `WasmHost` | `susi-error`, wasmer | feature planes | Wasm modules | instance | yes |
| `susi-sandbox` | Docker sandbox + `SusiConfig` + extension packs | `SandboxManager`, `SusiConfig`, `extensions` | core, paths, error, bollard | gawd/gmcp (prefer hooks) | Docker optional | config files | yes |
| `susi-tools` | Tool registry + MCP client adapters + `EngineHooks` port | `ToolRegistry`, `EngineHooks` | core, sandbox, native, rmcp/reqwest | gawd/gemi/gmcp **directly** (use hooks) | tools | registry | yes |
| `susi-agents` | Agent types + external peer adapters | `GawdAgent`, registries | core, sandbox | gemi engines | peers | registries | yes |
| `susi-gemi-models` | Model select / provision / catalogs | lifecycle, catalogs | core, sandbox, agents | gemi engines crate | catalogs | cache dirs | yes |
| `susi-gemi` | Inference adapters (Candle, HTTP, MCP-as-provider) | providers, engines | models, core, tools, agents | gmcp/gawd host | providers | model weights | yes |
| `susi-gawd-agents` | Fleet, safety/security, peers | agents, detectors | core, tools, gemi, sandbox | swarm/a2a (use dag/admin hooks) | agents | mission-local | yes |
| `susi-gawd-swarm` | AMA / DAG / cloud recovery | swarm dispatch | gawd-agents, core, tools, gemi | a2a wire | no | blackboard | yes |
| `susi-gawd-a2a` | A2A (`ra2a`) wire | task store, executor | gawd-agents | swarm | transport | tasks | yes |
| `susi-gawd` | Host facade: admin, evolution, reflex synth | re-exports + host modules | agents/swarm/a2a + infra | server/daemon | no | genome/reflexes | yes |
| `susi-gmcp` | MCP HTTP/stdio server + core tools + engine hooks impl | MCP surfaces, `SusiEngineHooks` | gawd, gemi, tools, core | daemon | MCP servers | sessions | yes |
| `susi-server` | Hyper HTTP adapters for GEMI/GMCP bind | bind helpers | gawd, gemi, tools | — | no | — | yes |
| `susi-daemon` | Persistent host: lock, ports, composition, rediscovery | `SusiDaemon`, `composition`, bootstrap | feature crates + server | — | no | lock/PID | yes |
| `susi` (root) | CLI + composition entry for workspace intents | `main`, CLI modules | daemon + feature crates | — | — | cwd workspace | yes |

Workspace crate cycles must remain **zero**. Cycles are broken with hook traits
(`EngineHooks`, `AdminHooks`, `HostHooks`, `dag_hooks`) wired at composition
roots — never by stuffing implementations into `susi-core`.

## Ports (traits) — create only at real substitution boundaries

| Port | Home | Implementations |
|------|------|-----------------|
| `Provider` | `susi-core` | Candle, HTTP OpenAI-compat, MCP-as-provider |
| `Tool` | `susi-core` | Core tools, MCP tools, observed wrappers |
| `CapabilityRegistry` | `susi-core` | process-wide catalog (locator today; prefer pass-by-ref in new code) |
| `EngineHooks` | `susi-tools` | `susi_gmcp::engine_hooks::SusiEngineHooks` |
| `AdminHooks` / `HostHooks` | gawd-agents / gawd-swarm | gawd host facades |
| `ProtocolDispatcher` / `CapabilityResolver` | `susi-gmcp` | MCP protocol |

Do **not** invent empty interfaces for every struct. Prefer the existing trait
surfaces and extension-pack capability strings.

## Composition roots

Exactly two process-entry assembly paths:

1. **CLI** — `susi_daemon::composition::wire_cli_substrate` (from `src/main.rs`)
   then command dispatch against cwd workspace.
2. **Daemon** — `SusiDaemon::run` → `wire_engine_hooks` →
   `bootstrap_zero_config_substrate` → bind host-contract ports 9090–9093.

Sequence (both paths, subset as applicable):

1. Load / seed configuration and extension packs.
2. Init hook traits (`EngineHooks`, and GAWD hooks on first swarm use).
3. Apply secrets surface (`~/.susi/cloud.env`) — never log secret bodies.
4. Construct / discover infrastructure (providers, MCP, peers).
5. Mount capabilities into `CapabilityRegistry`.
6. Attach transports (CLI ack, HTTP, MCP, UDP discovery).
7. Start / serve.

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

Permissions are **declared** now; hard enforcement (path jail, network
allowlist) is a later phase — never treat discovery as trust.

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

**Deferred:** relocating `SusiConfig` out of `susi-sandbox` into a dedicated
config/infra home (large move; documented here so it is not forgotten).

## Architecture tests

`tests/architecture_tests.rs` asserts:

- No workspace crate dependency cycles.
- Forbidden edges (notably `susi-core` and `susi-error` purity).
- Layer matrix from this document.

CI runs the full workspace test suite (includes these tests) and `cargo deny`.

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
- [ ] Full permission enforcement for packs (declared; enforce next).
- [ ] `SusiConfig` relocated out of sandbox.
- [ ] Versioned application event schemas beyond the existing bus.

Public boundaries use stable DTOs and versioned events where applicable.
Configuration authority for bundled JSON is documented in
[`config/README.md`](config/README.md).

`susi-gmcp` defaults to feature `tools-rich` (browser / tantivy / qdrant /
fastembed / syn AST tools). Use `--no-default-features` on that crate for
lighter local checks; shipped binaries keep the default.

## Migration roadmap (remaining)

1. **Stabilize contracts** — keep expanding ports only at real seams.
2. **Move I/O outward** — config crate; keep providers in GEMI.
3. **Tighten feature public APIs** — hide internals behind facades.
4. **Enforce pack permissions** at load/execute.
5. **CI** — reject layer violations (architecture tests already fail the build).
