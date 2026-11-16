# susi

![SUSI Version](https://img.shields.io/badge/version-v0.21.71-blue.svg) ![License](https://img.shields.io/badge/license-Apache%202.0-green.svg)

**[Two-minute start](#two-minute-start)** · **[Host contract](#host-contract)** · **[Where it stands](#where-it-stands)** · **[Architecture](docs/architecture.md)** · **[Evidence](docs/evidence.md)** · **[Integrate your app](docs/integrating-with-susi.md)** · **[Forking and contributing](CONTRIBUTING.md)**

Susi is a local-first AI OS socket: one Rust daemon exposes models, agents and
tools through stable protocols on ports `9090-9094`. Its three Apache-2.0 core
pillars are **GAWD** (agent coordination, A2A, ACP and parallel harness), **GEMI**
(inference routing), and **GMCP** (MCP and governed tools).

The core is a generic protocol boundary. Concrete model engines, providers,
workflow policies and product integrations belong in separately hosted packages,
including Susi Commercial Apps, connected through the same standard sockets.
Core ships no UI and bundles no plugin content. Existing implementations in core
are migration debt: each must have a working, compatible destination before
removal. The complete core must stay below 100,000 physical runtime Rust lines;
that goal remains unmet. Neither the architectural goal nor a passing build
certifies complete protocol conformance.

Use the socket to connect protocol clients, inspect routing decisions and run
authorized agents on the host. It is a substrate on the host, not a replacement
operating system. The core is Apache-2.0 and free forever, published one way,
and everyone is free to fork under the license.

## Two-minute start

```bash
# 1. Start the host daemon (binds 9090-9094, seeds ~/.susi/api_token)
susi start

# 2. See what you already have: hardware, local engines, cloud keys
susi ecosystem scan
susi keys check

# 3. Run an evidence-gated mission in the current directory
susi "analyze this repository and summarize its architecture"

# 4. List models through the authenticated HTTP socket
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
| **9091** | GEMI HTTP: chat/completion inference (`/v1/chat/completions`, `/v1/completions`, `/v1/embeddings`, `/v1/models`), Messages (`/v1/messages`), metrics (`/metrics`), host identity (`/.well-known/susi`) |
| **9092** | A2A UDP discovery |
| **9093** | GMCP HTTP alias (streamable / SSE) |
| **9094** | A2A HTTP (`/` JSON-RPC, `/stream` SSE, `/.well-known/agent-card.json`) |

The five ports shift together, never individually: `port_offset` in
`config.json` or `SUSI_PORT_OFFSET` (which wins) moves all of them, so offset
`100` gives `9190-9194`. Swarm gossip `9095` uses the same offset but is not
part of the contract.

> **Port collisions.** When no offset is pinned and a process other than this
> instance's susi holds one of the six ports the
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

The host exposes the five-port contract with bearer authentication and a
conformance checker. Current paths include evidence-gated missions, tool
receipts, an HMAC audit chain and a Wasm interpreter. These paths have separate
capability and qualification checks; availability does not establish complete
standards compliance or qualify an artifact for production.

The current default build uses an external inference plug or configured HTTP
engine/provider. Embeddings and browser page capture are plug ports and report
a typed capability gap when no plug is wired. In-process model delivery remains
a documented gap. Pattern-based governance vetoes are not a sandbox boundary.
See [current limitations](docs/evidence.md) and [delivery acceptance](docs/step1-acceptance.md).

Full acceptance requires the exact combined source and artifact to pass the
installed journey, promotion, health and rollback checks. The separate warm
`cargo fast-release` budget is <10 s; it does not replace full release
qualification. The cold compile target is <60 s. Report current-tree measurements
and platform coverage explicitly; retained historical timings are not current
qualification. Core releases target native x86_64 Windows and Linux; other
platforms remain portable source without release qualification.

## Install

Agents obtain an identified native artifact and invoke its Rust installer with
`susi install`. It discovers the user profile, verifies the staged executable,
and installs without replacing a different existing executable. No shell script,
hosting-service connection or user-written configuration is required for placement.
Susi never downloads a model by itself: cloud models work right away, and
`susi os manage model install starter` downloads the licensed local starter
model (about 429 MB) when you ask. A local goal without it says so and names
that command.
Native installation does not by itself qualify a candidate for production;
qualification, promotion and rollback have separate evidence gates.

From source, install with `install.sh` (x86_64 Linux) or
`install.ps1` (x86_64 Windows) from a clone of this repository. This needs Git
and a Rust toolchain from [rustup](https://rustup.rs): the release tag pins its
compiler in `rust-toolchain.toml` (for example, 1.99.0), and when that
toolchain is missing the installer stops and prints the `rustup toolchain
install` command. It never installs software for you and downloads no binary.

Replace `REPOSITORY_URL` below with the selected source repository URL.

```sh
git clone REPOSITORY_URL susi && cd susi && ./install.sh
```

```powershell
git clone REPOSITORY_URL susi; cd susi; ./install.ps1
```

The installer builds the newest release tag of the clone's `origin` (or of
`SUSI_SOURCE_REPO`, the core repository's git URL, when it runs outside a
clone; `SUSI_RELEASE_TAG` pins a release) with the settings in that release's
committed record, and installs it only when it reports the tag's version. A
cold build takes several minutes. Releases, their records and the reference
checksums are described in [docs/releases.md](docs/releases.md). Platform scope
and qualification are recorded in [docs/evidence.md](docs/evidence.md#platforms).

### Developing susi

Agents own authorized development, verification, integration and production
qualification under the [active engineering contract](AGENTS.md). Work in an
isolated worktree with disjoint scopes (`cargo local-workflow start <agent>
--scope <path>`, `cargo local-build debug`) and private build/runtime state.

Use portable Git transport and reconcile the current remote before work and
commits. Commit, push, merge and pull meaningful work about every ten minutes.
Each commit carries `Task: T-<AGENT>-<n>`. Main is unqualified development;
release integration verifies the actual combined tree before a release ref
moves, and published release tags remain immutable. Keep source publication
separate from production promotion, health checks and rollback.

Operational history, completed tasks and growing evidence belong outside source,
in a local database when available; absent history never proves acceptance.
Keep only necessary active coordination while the native tooling needs it.
Update the current contract in place instead of extending mandate histories.
Preserve mandatory licenses, attribution, live authorization/recovery data and
artifact-bound release provenance. The [workflow guide](docs/evidence.md#building-susi-the-workflow)
describes available commands and migration limits.

## Read next

- [docs/architecture.md](docs/architecture.md): pillars, Tier S invariants, plane bus and its loopback rendezvous, the plug bridge to the commercial suite, the lean-core direction, the command reference.
- [docs/evidence.md](docs/evidence.md): verified claims, platforms, the delivery workflow and every admitted gap.
- [ARCHITECTURE.md](ARCHITECTURE.md): the enforceable crate-boundary contract (`tests/architecture_tests.rs`).
- [docs/gmcp-roadmap.md](docs/gmcp-roadmap.md): the standards-traceable GMCP completion program; the genome ledgers under [`.agents/`](.agents/) are described in [docs/architecture.md](docs/architecture.md#genome).
- [docs/lean-core.md](docs/lean-core.md#repository-line-accounting): tracked repository line accounting by category (separating first-party runtime Rust from tests, tooling, vendored sources, and audit/evidence trails), measured and printed by [`tests/lean_core_ceilings.rs`](tests/lean_core_ceilings.rs). The complete socket ceiling only decreases until the under-100,000 goal is met.
- [docs/extension-pack.md](docs/extension-pack.md), [docs/harness.md](docs/harness.md), [VISION.md](VISION.md), [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Apache License 2.0. See [LICENSE](LICENSE). Provided as-is, without warranty; users run the engine and its agents at their own risk.

The repository is published one way: the designated publisher alone holds push
authority, and authorized agents act within that scope. This grants no additional
write access; no outside pull request, patch or commit is accepted. You are free
to fork under the license, which grants code rights and no right to the Susi name
or marks. See [CONTRIBUTING.md](CONTRIBUTING.md).
