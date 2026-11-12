# susi

![SUSI Version](https://img.shields.io/badge/version-v0.21.65-blue.svg) ![License](https://img.shields.io/badge/license-Apache%202.0-green.svg)

**[Two-minute start](#two-minute-start)** · **[Host contract](#host-contract)** · **[Where it stands](#where-it-stands)** · **[Architecture](docs/architecture.md)** · **[Evidence](docs/evidence.md)** · **[Integrate your app](docs/integrating-with-susi.md)** · **[Contributing](CONTRIBUTING.md)**

Susi is an open, local-first AI operating substrate: one Rust daemon on your
machine that puts the models, agents and tools you already have behind a
standards socket on ports `9090-9094`, so any MCP client, ACP agent,
OpenAI/Anthropic SDK or A2A peer talks to all of them the same way. It is built
around **five Apache-2.0 core pillars** — **GAWD** (evidence-gated swarm & A2A
coordination), **GEMI** (local/cloud model inference router), **GMCP** (MCP
protocol & tool plane), **ACP** (Agent Client Protocol / Agent Communication
Protocol standard open socket), and **Susi Harness** (recursive driver/engine
with built-in parallel multi-agent workflow) — routes work across local engines
(Ollama, llama.cpp, vLLM) and cloud providers by evidence, lets agents declare a
mission complete only with receipts, and seals what they did into an append-only
HMAC-SHA256 audit chain. The core is Apache-2.0 and free forever.

Scoped claim (Mandate 45): Susi is an evidence-gated agent-of-agents substrate
*on* your host, not a replacement operating system and not a chatbot wrapper.
"RSI" here means two audited loops, only one of which runs unattended; the
exact scope is in [What RSI means here](docs/architecture.md#what-rsi-means-here).

## Who it is for

- **Developers** who want one local endpoint for every model and tool, with an
  inspectable routing decision (`susi os route`) instead of a vendor default.
- **Teams building products on Susi** from the outside, through the fixed port
  contract, never by forking it ([integration guide](docs/integrating-with-susi.md),
  [conformance checker](docs/host-contract-conformance.md)).
- **Agents and their operators** who need missions grounded in receipts, a
  governed tool path and an audit trail that survives a crash.

Core is a pure CLI plus standards socket: it ships no UI and bundles no plugin
content; the desktop, every visual view, enterprise SSO/RBAC/FinOps governance,
turnkey workflows and curated plugin packs live in Susi Commercial Apps
(`intellibitz/susi-commercial-apps` — closed-source, licensed, and paid corporate
software), which reaches core over the same public protocols. susi core is the
free open-source socket. It speaks the standard host protocols and admits plugs.
It does not ship a third-party product implementation; the third party or Susi
Commercial Apps writes that plug.

## Two-minute start

```bash
# 1. Start the host daemon (binds 9090-9094, seeds ~/.susi/api_token)
susi start

# 2. See what you already have: hardware, local engines, cloud keys
susi ecosystem scan
susi keys check

# 3. Run an evidence-gated mission in the current directory
susi "analyze this repository and summarize its architecture"

# 4. Call the OpenAI-compatible socket from curl or any OpenAI SDK
curl -s http://127.0.0.1:9091/v1/models \
  -H "Authorization: Bearer $(cat ~/.susi/api_token)"

# 5. Ask which model susi trusts for which work, and why
susi brain                     # SUSI_BUDGET=low|balanced|max turns the cost dial
```

Every HTTP contract port needs `Authorization: Bearer <token>` except
`/health`. `susi stop` / `susi restart` are deterministic host commands.
The full command reference is in
[docs/architecture.md](docs/architecture.md#operating-the-socket-command-reference).

## Host contract

External clients can trust these ports; the daemon never silently drifts them.
Verify a running host with `cargo run -p susi-conformance`.

| Port | Surface |
|------|---------|
| **9090** | GMCP / MCP HTTP (`/mcp`; `/messages` alias) |
| **9091** | GEMI HTTP: OpenAI-compatible inference (`/v1/chat/completions`, `/v1/completions`, `/v1/embeddings`, `/v1/models`), Anthropic Messages (`/v1/messages`), Prometheus (`/metrics`), host identity (`/.well-known/susi`) |
| **9092** | A2A UDP discovery |
| **9093** | GMCP HTTP alias (streamable / SSE) |
| **9094** | A2A HTTP (`/` JSON-RPC, `/stream` SSE, `/.well-known/agent-card.json`) |

The five ports shift together, never individually: `port_offset` in
`config.json` or `SUSI_PORT_OFFSET` (which wins) moves all of them, so offset
`100` gives `9190-9194`. Swarm gossip `9095` uses the same offset but is not
part of the contract.

> **Port collisions.** `9090` is also the Prometheus server default and
> `9092-9094` are common Kafka listener ports. When no offset is pinned and a
> process other than this instance's susi holds one of the six ports the
> daemon binds (`9090-9094` and gossip `9095`), the daemon first evicts only a
> stale susi process of its own, then moves the whole block to the first free
> offset of 100 (`9190-9195`, then `9290-9295`, up to offset `2000`). It logs
> which port was held and by which process, and publishes the chosen offset
> with its pid in `ports.json` in its config directory; `susi start`, `susi os
> status` and every client of the same instance find it there, and the start
> banner says the block is a fallback. The ports never move one at a time and
> never become ephemeral. To choose the block yourself, pin it: a
> `SUSI_PORT_OFFSET` value (`0` included) or a non-zero `port_offset` in
> `config.json` is bound exactly, and the daemon refuses to start if that block
> is taken. A second instance on one host also needs its own root:
> `SUSI_HOME=~/.susi-b SUSI_PORT_OFFSET=100 susi start`.

## Where it stands

Production, held in source and verified on a running host:
the five-port contract with bearer auth and `susi-conformance`; `susi crown`
(exit non-zero if a Tier S pillar fails: truth, evidence, swarm, audit,
sandbox, host contract and the rest); evidence-gated missions with live tool
receipts and the HMAC audit chain; Wasm reflexes and the crown's sandbox
probes on Wasmi, the pure-Rust interpreter (no Wasmtime is linked); the clean
Windows x86_64 installed journey and production releases C through F with
promotion and rollback, recorded 2026-11-02 on builds that still carried the
in-process engine ([docs/step1-acceptance.md](docs/step1-acceptance.md)).

Gated, measured but not yet accepted: Step 1 of the CEO's vision stays open
until the final combined source publishes an acceptance receipt and the exact
production artifact passes the installed journey with A/B promotion, health
and rollback shown; the user-response budgets are met on a quiet host for one
build only and production qualification does not yet enforce them; the
compile budget is 60 s and the last idle measurement is 81.7 s
([docs/lean-core.md](docs/lean-core.md)).

Admitted, not delivered in core: the in-process GGUF model runtime left core
with no destination yet (two `tests/zc_matrix.rs` tests ignored by CEO
decision); a default build serves local in-process inference only through a
plug wired on the plane bus (the commercial `susi-plug-host`) and otherwise
routes the request to the HTTP engines you run or to cloud providers;
embeddings and browser page capture are likewise plug ports and, with no plug
wired, report a typed capability gap. Governance vetoes catch known-dangerous
shapes by pattern; they are not a sandbox boundary. Core releases only for
x86_64 Windows and x86_64 Linux; other targets stay portable in source and are
not released by core. Every gap with its record:
[docs/evidence.md](docs/evidence.md#admitted-gaps-on-record-2026-11-08).

## Install

Agents obtain an identified native artifact and invoke its Rust installer with
`susi install`. It discovers the user profile, verifies the staged executable,
and installs without replacing a different existing executable. No shell script,
GitHub connection or user-written configuration is required for placement.
Susi never downloads a model by itself: cloud models work right away, and
`susi os manage model install starter` downloads the licensed local starter
model (about 429 MB) when you ask. A local goal without it says so and names
that command.
Native installation does not by itself qualify a candidate for production;
qualification, promotion and rollback have separate evidence gates.

From source, a person installs with `install.sh` (x86_64 Linux) or
`install.ps1` (x86_64 Windows) from a clone of this repository. They need git
and a Rust toolchain from [rustup](https://rustup.rs): the release tag pins its
compiler in `rust-toolchain.toml` (1.99.0 for v0.21.52), and when that
toolchain is missing the installer stops and prints the `rustup toolchain
install` command. It never installs software for you and downloads no binary.

```sh
git clone https://github.com/intellibitz/susi.git && cd susi && ./install.sh
```

```powershell
git clone https://github.com/intellibitz/susi.git; cd susi; ./install.ps1
```

The installer builds the newest release tag of the clone's `origin` (or of
`SUSI_SOURCE_REPO`, the core repository's git URL, when it runs outside a
clone; `SUSI_RELEASE_TAG` pins a release) with the settings in that release's
committed record, and installs it only when it reports the tag's version. A
cold build takes several minutes. Releases, their records and the reference
checksums are described in [docs/releases.md](docs/releases.md). Platform scope
and qualification are recorded in [docs/evidence.md](docs/evidence.md#platforms).

### Developing susi

The CEO never inspects code; agents own development, verification, integration
and production qualification under [`AGENTS.md`](AGENTS.md) (Mandates 49–76),
[`docs/ceo-mandate-74.md`](docs/ceo-mandate-74.md),
[`docs/ceo-mandate-75.md`](docs/ceo-mandate-75.md) and
[`docs/ceo-mandate-76.md`](docs/ceo-mandate-76.md) (agents collaborate through the
Susi Board). Work happens in isolated
worktrees with disjoint scopes (`cargo local-workflow start <agent> --scope <path>`,
`cargo local-build debug`) and remote pull-and-merge reconciliation at every
workflow step over portable Git storage (GitHub is storage only: zero GitHub
Actions, zero hosted PRs). Susi core targets a `<10s` warm `build + test + release`
pipeline budget (`docs/lean-core.md`). Both `intellibitz/susi` and
`intellibitz/susi-commercial-apps` operate on the **Raw Source (`main`) ->
Distill (`release`)** model, and cloud raw-source feature pickup
([`docs/harvest.md`](docs/harvest.md)) enforces **Pure `Apache-2.0` ONLY**
compliance while routing 5-pillar primitives to Core and commercial add-ons to
`susi-commercial-apps`. Each commit carries `Task: T-<AGENT>-<n>`
with its record in [`.agents/tasks/`](.agents/tasks/), features land
continuously on `main` and are deterministically distilled by Susi into
qualified immutable releases (`releases/<tag>.json`), and release integration
verifies the actual combined tree before a release ref moves;
[`.agents/lanes/`](.agents/lanes/) documents history and never substitutes
for a reservation. The full path, with the native Windows notes, is in
[docs/evidence.md](docs/evidence.md#building-susi-the-workflow-mandates-4972).

## Read next

- [docs/architecture.md](docs/architecture.md): pillars, Tier S invariants, plane bus and its loopback rendezvous, the plug bridge to the commercial suite, the lean-core direction, the command reference.
- [docs/evidence.md](docs/evidence.md): verified claims, platforms, the delivery workflow and every admitted gap.
- [docs/gate-receipts.md](docs/gate-receipts.md): every gated landing's receipt under [`.agents/receipts/`](.agents/receipts/), and how to re-run one on a clean clone with `cargo xtask verify-receipt <commit>`.
- [ARCHITECTURE.md](ARCHITECTURE.md): the enforceable crate-boundary contract (`tests/architecture_tests.rs`).
- [docs/gmcp-roadmap.md](docs/gmcp-roadmap.md): the standards-traceable GMCP completion program; the genome ledgers under [`.agents/`](.agents/) are described in [docs/architecture.md](docs/architecture.md#genome).
- [docs/extension-pack.md](docs/extension-pack.md), [docs/harness.md](docs/harness.md), [VISION.md](VISION.md), [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Apache License 2.0. See [LICENSE](LICENSE). Provided as-is, without warranty; users run the engine and its agents at their own risk.
