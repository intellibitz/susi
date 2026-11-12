**START HERE: read the CEO mandates below, inspect local state, and reserve an isolated worktree before editing.**

## CEO Mandate 78 (2026-11-12): Never Reinvent the Wheel — Reuse Apache-2.0 Work First

Read [CEO Mandate 78](docs/ceo-mandate-78.md). It continues Mandate 75: raw
features are dumped into `main` and distilled into releases, and agents look
for existing work before writing new.

- **Look before you build:** before writing new code, look first inside Susi
  (both repositories), then at Apache-2.0 work of every kind — code, libraries,
  frameworks, tools, products and services (`local-harvest raw-features` /
  `route`) — then at what the locked dependency graph already has. Harvest
  needed source; admit an existing product or service as a plug instead of
  rebuilding it. Build only when none is suitable, and say why.
- **Reuse the right way:** harvest under Mandate 75's rules (pure `Apache-2.0`,
  pinned commit, manifest, notices, `Origin:` trailers), take only the part
  that is needed, and post the upstream target on the Susi Board first.
- **Delete what reuse replaces** in the same change; never wrap a duplicate.
- **Smaller and faster, never the opposite:** reuse stays inside the lean
  ceilings, the `<10s` budget, Rust-only and the dependency ceilings. It is not
  permission to add dependencies.
- **Record where you looked, before your first commit,** in the task record's
  `reuse` object: `decision` (`existing`, `harvest`, `dependency` or `build`),
  `searched`, and the `source` of a harvest or the `reason` for a build. The
  commit hook refuses a commit for a feature-sized task that has not
  (`susi-commercial workflow reuse`; sizes `xs` and `s` are exempt;
  `SUSI_ALLOW_NO_REUSE=1` only knowingly). `distill lint` counts the same
  gaps, as advice.

## CEO Mandate 77 (2026-11-12): One Isolated Working Directory per Agent (Agent Roots)

Read [CEO Mandate 77](docs/ceo-mandate-77.md). Every agent works only under its
own root: one directory per agent per machine, named for the agent.

- **Find or register your root first:** `susi-commercial workflow root show`,
  or `susi-commercial workflow root init --agent <name>` (default
  `~/susi-agents/<name>`). A root is marked by a `.susi-agent-root` file naming
  its owner; existing roots are registered in place, never moved.
- **Everything you create lives under it:** your own clones of both
  repositories, every worktree, build target, private `SUSI_HOME` and scratch
  file.
- **Never write in another agent's root or the CEO's working directory.** You
  may read it and fetch from it; to change something there, ask the owner on
  the Susi Board.
- **Enforced:** `workflow start` and the commit guard refuse to work inside a
  root that belongs to a different agent. A checkout in no registered root is
  reported, not refused.

## CEO Mandate 76 (2026-11-12): Multi-Agent Collaboration through the Susi Board

Read [CEO Mandate 76](docs/ceo-mandate-76.md). Every agent collaborates with
every other agent through the Susi Board; the CEO is not a message relay.
**Agents always work in collaborative mode and use the Susi-native board,
whether one agent or many is working** (CEO directive 2026-11-12): never assume
you are alone. Board commands work from a checkout of this repository too
(they use the `susi-commercial-apps` clone under your agent root), and
`susi-commercial workflow check` shows the board when you start.

- **Read the board** at session start, before claiming a scope, before every
  publish to `main`, and before cutting a release. A stop note halts publishing.
- **Post on the board:** a Claim before the first edit (task ID, paths, worktree,
  acceptance), Landed after the commit is on `origin`, and Cut, Heads-up,
  Request, Handoff, Decision or Blocked when they apply.
- **Append only:** add the next numbered item, re-check the number after
  appending, never edit or delete another agent's item.
- **Respect claims and answer each other:** never edit inside another agent's
  open claim, ask the owner on the board, and answer what is addressed to you
  before starting new work.
- **Truthful, confidential, portable:** only verified facts; no credentials or
  commercial source on the board; plain text with zero GitHub or hosted-service
  dependency. The board coordinates; Git stays the record; it adds no gate.

## CEO Mandate 75 (2026-11-12): Raw Source (main) & Distill (release), Cloud Raw-Source Pickup, and Pure Apache-2.0 Compliance

Read [CEO Mandate 75](docs/ceo-mandate-75.md) first; it builds on Mandate 74 and
wins where older text differs.

- **Raw Source (`main`) -> Distill (`release`) across both repositories:** Both
  `intellibitz/susi` and `intellibitz/susi-commercial-apps` run strictly on the
  **Raw Source (`main`) -> Distill (`release`)** pipeline. Raw features (both
  agent-authored and cloud-harvested) land freely and continuously on `main` with
  remote pull and merge reconciliation at every workflow step and structured
  final-paragraph commit trailers (`Task:`, `Feature:`, and when harvested
  `Harvest:`, `Origin: <url>@<sha>:<path>`, `Origin-License: Apache-2.0`). Susi —
  never GitHub — deterministically distills verified features into conflict-free
  `release` candidate lines (`refs/candidates/release/*`,
  `refs/candidates/distill/*`, one-version-one-cut `release_version_guard`
  claims) and immutable `vX.Y.Z` snapshots.
- **Cloud raw-source feature pickup (`Core` vs. `Commercial Apps`):** Use
  `local-harvest raw-features`, `local-harvest route`, and `local-harvest pickup`
  (`docs/harvest.md`) to freely discover and integrate capabilities from cloud raw
  sources without reinventing the wheel: route lean, zero-UI, zero-vendor-product
  capabilities inside the **5 open-core pillars** (`GAWD`, `GEMI`, `GMCP`, `ACP`,
  `Susi Harness`) within `<10s` (`total_ms < 10_000`) and `lean_core_ceilings` to
  `intellibitz/susi`; route all product features, UIs, enterprise packs,
  connectors, SSO/RBAC, FinOps and turnkey workflows exclusively to closed,
  licensed, paid `intellibitz/susi-commercial-apps`.
- **PURE `Apache-2.0` ONLY for cloud harvest compliance:** Across both `susi` and
  `susi-commercial-apps`, cloud raw-source feature pickup and harvesting accepts
  **ONLY `Apache-2.0`** (pure `Apache-2.0`, or dual-licensed `Apache-2.0 OR ...`
  electing and binding `"Apache-2.0"` exclusively). Standalone `MIT`-only,
  `BSD-2-Clause`-only, `BSD-3-Clause`-only, `Apache-2.0 WITH LLVM-exception`
  (unless pure `Apache-2.0` is offered via `OR`), `AND` combinations, `ISC`,
  `MPL`, `GPL`/`LGPL`/`AGPL`, `SSPL`/`BUSL`, source-available, and unlicensed raw
  source are hard-refused. Every harvested file carries full Apache-2.0 §4(a)–(d)
  compliance (`LICENSES/Apache-2.0.txt`, `Modified by` notice on modified/ported
  files, preserved upstream notices, and deterministic `THIRD_PARTY_NOTICES`).
  Existing `deny.toml` crate-graph closure remains truthful for `Cargo.lock`
  (Mandate 68), while pure-`Apache-2.0` raw-source pickup is preferred over adding
  new external crate dependencies.
- **Multi-agent Git & Susi Board collaboration:** `Claude`, `Codex`,
  `Antigravity` (`Carbon`), `Grok` and `Susi Harness` collaborate additively over
  portable Git storage (`origin`) and the shared Susi coordination board,
  deconflicting upstream harvest targets before import and preserving peer work.

## CEO Mandate 74 (2026-11-11): <10s core, 5 pillars, remote pull & merge, distilled releases, corporate commercial boundary

Read [CEO Mandate 74](docs/ceo-mandate-74.md) alongside
[CEO Mandate 75](docs/ceo-mandate-75.md) (Mandate 75 refines cloud raw-source
pickup to Pure `Apache-2.0` only); they win where older text
differs. **Agent reminder:** work in your own isolated worktree with remote pull
and merge at every step, keep Susi core build+test+release under 10 seconds across
its 5 Apache-2.0 pillars, dump labelled work straight into `main` with no gates
and no PRs, let Susi distill releases with conflict-free parallel picks, harvest
pure Apache-2.0 code with full attribution, keep all add-ons closed/licensed/paid in
`susi-commercial-apps`, and treat GitHub as a storage bucket only.

- **Blazing-fast `<10s` Susi core budget:** Core build + verification test suite
  + distilled release artifact and SHA-256 provenance manifest must complete in
  **under 10 seconds (`< 10.0s` / `total_ms < 10_000` warm pipeline budget)**,
  recording `build_ms`, `test_ms`, `release_ms`, `total_ms` and durable evidence
  while reporting cold vs. warm timings separately and honestly (Mandates 1, 62, 69).
  The `<10s` budget is the warm fast check (`cargo fast-release`); it is not
  the full workspace suite, which takes minutes and is what qualifies a
  release. Report both, never one as the other.
- **Continuous `main` & deterministic release distillation:** `main` is an
  append-only, ungated feature dump (no gates, no workflows, no pull requests).
  Label every dump (one feature per commit series, `Task: T-<AGENT>-<n>` trailers,
  `.agents/tasks/` record). Susi, never GitHub, deterministically distills
  qualified releases (`releases/<tag>.json`, `.agents/releases/<name>.json` on
  `refs/candidates/release/<name>`, and immutable release tags): disjoint lanes,
  atomic claim refs, `cherry-pick -x`, exact-tree qualification, atomic
  fast-forward pushes.
- **Remote pull and merge at every workflow step:** Every step (`start`, `sync`,
  `commit`, `distill`, `ship`, `release`, and parallel Susi Harness steps)
  supports remote `fetch` + pull and merge reconciliation with `origin` when
  reachable (offline-safe when disconnected) so distributed agents and laptops
  stay continuously reconciled.
  **Every agent must work on the latest remote** (tightened 2026-11-12): fetch
  and merge `origin/main` before starting work, when resuming work, and before
  every commit. `susi-commercial workflow check` (run it first, when you start
  and when you resume) and `workflow fresh --pull` do the pull for you on a
  worktree with no uncommitted changes, and never touch one that has.
  `susi-commercial workflow fresh` reports the lag; the commit
  guard notes any lag and refuses a commit more than 20 commits behind
  (`git config susi.maxBehind <n>` changes the limit; an unreachable remote
  never refuses; a merge is never refused).
- **GitHub is portable Git storage only:** Pure Git transport (`origin` for
  `susi` and `susi-commercial-apps`); zero GitHub Actions, zero hosted PR/status
  gates, zero `gh` CLI or GitHub API lock-in; moving clouds is a remote URL change.
- **5 Apache-2.0 free-forever core pillars:** **GAWD** (swarm/A2A coordination),
  **GEMI** (model inference router), **GMCP** (MCP protocol & tool plane),
  **ACP** (Agent Client Protocol / Agent Communication Protocol standard socket),
  and **Susi Harness** (driver & engine with built-in parallel multi-agent
  workflow; becomes default driver only on measured parity).
- **Closed, licensed, paid corporate commercial suite:** All add-on features,
  UIs, enterprise packs, SSO/RBAC, FinOps, corporate fleet governance, and
  turnkey workflows go **exclusively** into **Susi Commercial Apps**
  (`intellibitz/susi-commercial-apps`), which is closed-source, licensed
  (`SUSI-ENT-v1` / Ed25519 `SUSI-CUST-v1`), and paid software for commercial
  entities and corporates.
- **Attributed Pure Apache-2.0 harvesting (Mandate 75):** Any pure `Apache-2.0`
  repository (or dual-licensed `Apache-2.0 OR ...` electing `"Apache-2.0"`
  exclusively) anywhere may be harvested with quarantine intake, per-file license
  verification, a harvest manifest, Apache-2.0 section 4 notices and `Origin:`
  trailers; standalone MIT/BSD, `Apache-2.0 WITH LLVM-exception`, `AND`, copyleft
  and unlicensed raw source is refused.

## CEO operating mandate (2026-11-10, Mandate 73)

Read [the consolidated CEO agent mandate](docs/ceo-agent-mandate.md) before work.
Three isolated laptops each run the CEO's agent roles, **dot, opus, carbon and grok**: role names, not vendors and not a head count. The agents actually present on a machine are those with a registered agent root (Mandate 77). An **available authorized driver** coordinates local agents on each laptop:
dot, duet, another agent, or Susi harness when its capabilities are verified.
Cloud sessions are independent and fully autonomous
within the user's authorization. Susi's master brain owns distillation, task and
feature manifests, branch composition and serialized integration/publication.
The recursion adapter applies to every Susi container component: a driver can
be an engine and an engine a driver, nested through capability contracts with
cycle/termination checks, resource budgets and no permission amplification.
**Main has no mandatory pre-push gates and is unqualified development. Full
exact-tree gates apply to release branches and snapshots.** There is no arbitrary
agent-count limit or fixed push-rate limit. Admit work
against measured hardware capacity, and keep private worktrees, targets, runtime
homes and identities. Agents may contribute to main as often as work is ready. Reconcile current heads,
preserve accepted work and publish with normal atomic fast-forward updates.
Git is portable storage; GitHub is storage only, with no GitHub Actions.
The only remote branch on GitHub is `refs/heads/main` (CEO directives
2026-11-11): agents publish their work to the non-branch ref
`refs/agents/<agent>` (`git push origin local-agent/<agent>:refs/agents/<agent>`,
see `docs/agent-refs.md`); never push `refs/heads/local-agent/*`,
`refs/heads/release/*`, `refs/heads/release-staging/*`, `refs/heads/distill/*`,
`refs/heads/wip/*`, or any other branch head, and never open, merge, or close a
pull request. A release line lives on storage as the non-branch ref
`refs/candidates/release/<name>` (fast-forward only, never deleted,
`docs/release-lanes.md`); a `refs/heads/release/*` branch stays local. Snapshots
and releases publish as immutable tags (`refs/tags/susi-distill/<name>`,
`refs/tags/v<version>`).
Development branches may advance without a mandatory gate; release candidates
require qualification and published release tags are
immutable snapshots with source, license, toolchain, tests and artifact provenance.
Preserve GAWD, GEMI, GMCP, ACP and the Susi harness as lean Apache-2.0 core, commercial
confidentiality, existing access controls, safety, production health and rollback.
This is a mandate, not a claim that all agents, providers or workflow features
are deployed. No security setting, credential grant or branch protection changes
are authorized by this document. The complete contract identifies superseded
remote cooldown/single-laptop/main-gate restrictions and preserves release gates,
access controls and history-preservation safeguards.
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

**Mandate 62 execution criteria.** The CEO sets the aspiration. The workflow and harness are the foundation through which agents build, test, operate and improve Susi for CEOs, in successive groups of 100 steps. Step 1 stays open until measured user response meets declared millisecond budgets and a genuine source change reaches the verified live executable in under 60 seconds. Record cold and warm build, test, startup, goal acknowledgment, first useful output and completion separately. Time the complete local delivery loop from change ready through tests, merge, promotion and observation of changed product behavior. Record inputs, repetitions, failures and rollback. Prefer simple stable public protocols and independently installable third-party plugins. Publish conformance tests for permissions, discovery, errors and compatibility. Prove a plugin update by showing that a pack hosted by susi-commercial-apps is admitted over the socket without rebuilding Susi; preserve user capabilities, and measure removed source, dependency reach, executable size and wall-clock cost before claiming bloat reduction. Agents own isolated parallel delivery, continue committing and pushing verified work, and wrap up scoped changes at 10% weekly allowance remaining. Source publication never substitutes for production qualification.

The constitution is `.agents/identity.json`, compiled into the runtime. Mandates
62–72 express the CEO's eleven requirements; revised Mandates 43–54 and 61 remove
conflicting historical shell/GitHub/human-engineering prerequisites. Existing
scripts, remote workflows and installed binaries can still implement the old
behavior: treat that as migration work, not as authority to disregard the CEO.
These are requirements, **not claims of completed implementation**. Consult
`docs/native-rust-direction.md` and live verification for current status.

**Retired and merged mandates (CEO decision 2026-11-12).** Mandate IDs are never
renumbered or reused; a retired ID stays in `.agents/identity.json` with a
pointer. Merged: 2, 7 and 8 into 1 (truth); 3 into 69 (bloat); 36 into 25
(destructive-command guard); 38 into 10 (secrets); 44 into 70 (zero
configuration); 43 into 63 (Rust-only). Retired: 28, 29 and 30, which mandated
`tokio`, `rayon` and `parking_lot`; the goal is kept in Mandate 5 and the choice
of library follows the dependency ceilings. Reworded: 5 and 35 (goal, not
library), 54 (a release gate, never a main gate, no pull-request adapters), 68
(dependency allowlist vs. Apache-2.0-only for copied source), 72, 73 and 74.

| Mandate | Required outcome |
| --- | --- |
| 62 | Agent-only end-to-end delivery; no CEO code inspection or technical repair. |
| 63 | Rust-only Susi runtime, build/install/release tooling and shipped implementation dependencies. Rust facades do not turn C/C++ into Rust. |
| 64 | Native Windows is first-class: MSVC and native OS APIs, no Bash/WSL/MSYS/Python/Node/container prerequisite. |
| 65 | Parallel agents with private source, build and runtime state, disjoint scopes, serialized integration, gated releases and measured speed. |
| 66 | Development, integration and debug releases work locally without GitHub; hosting/publication are optional. |
| 67 | Separate debug and optimized production artifacts; evidence-gated atomic promotion, health checks and verified rollback. |
| 68 | Apache-2.0, MIT, BSD-2-Clause and BSD-3-Clause only for distributed components; correct SPDX interpretation, preserved notices and no audit exclusions hiding shipped violations. |
| 69 | Zero bloat through justified capabilities and measured size, memory, latency and build budgets. |
| 70 | Zero required user-written configuration; discovery, defaults and repair are agents' responsibility. |
| 71 | **Susi: AI that earns its next upgrade.** Promote demonstrated improvement, with durable evidence and rollback. |
| 72 | **Platform scope, licensing and budget:** Susi core is Apache-2.0 and free forever, with releases and CI on vanilla Windows x86_64 and Linux x86_64 only (portable source kept); Susi Commercial Apps is private/paid and also builds, tests and releases for x86_64 Windows and Linux only; its macOS/ARM64 support is paused (CEO decision 2026-11-05) until the CEO reopens it. |
| 73 | **Independent agents and Susi integration:** three isolated laptops with an available authorized local driver, recursive driver/engine adapters across container components, independent autonomous cloud sessions, capacity-based unlimited development contributions, master-brain distillation, portable Git storage and immutable release snapshots. |
| 74 | **Autonomous CTO ecosystem mandate (2026-11-11):** `<10s` warm core build+test+release budget (`total_ms < 10_000`); 5 Apache-2.0 free-forever core pillars (**GAWD**, **GEMI**, **GMCP**, **ACP**, **Susi Harness** with parallel multi-agent workflow); remote pull and merge at every workflow step over portable Git storage (zero GitHub Actions/PRs/API); ungated labelled `main` deterministically distilled into immutable releases; attributed Apache-2.0 harvesting; all add-ons/UIs/SSO/RBAC/FinOps closed, licensed, and paid for corporates in `susi-commercial-apps`. |
| 75 | **Raw Source (`main`) & Distill (`release`), Cloud Feature Pickup, and Pure `Apache-2.0` Compliance (2026-11-12):** symmetric Raw Source (`main`) -> Distill (`release`) pipeline across `susi` and `susi-commercial-apps` with one-version-one-cut guards; `local-harvest raw-features` / `route` / `pickup` routing 5-pillar `<10s` capabilities to Core and all UIs/packs/connectors/workflows to `susi-commercial-apps`; **Pure `Apache-2.0` ONLY** cloud harvest compliance (full §4(a)–(d) notices, refusing standalone MIT/BSD, LLVM-exception, `AND`, copyleft and unlicensed raw source); additive multi-agent (`Claude`, `Codex`, `Carbon`) Git and Susi Board collaboration. |
| 76 | **Multi-agent collaboration through the Susi Board (2026-11-12):** every agent reads the Susi Board at session start and before claiming, publishing or cutting a release; posts Claim, Landed, Cut, Heads-up, Request, Handoff, Decision and Blocked items; appends only and never rewrites another agent's item; respects open claims and answers requests addressed to it; keeps the board truthful, free of secrets and commercial source, and free of any GitHub or hosted-service dependency. The board coordinates, Git stays the record, and no gate is added ([`docs/ceo-mandate-76.md`](docs/ceo-mandate-76.md)). |
| 77 | **One isolated working directory per agent (2026-11-12):** every agent has exactly one root directory per machine, named for the agent and marked with `.susi-agent-root`; all of its clones, worktrees, build targets and scratch files live under it; it never creates, changes or deletes anything under another agent's root or the CEO's working directory, and asks the owner on the Susi Board instead. `susi-commercial workflow root show | init` finds and registers a root; `workflow start` and the commit guard refuse to work inside another agent's root ([`docs/ceo-mandate-77.md`](docs/ceo-mandate-77.md)). |
| 78 | **Never reinvent the wheel (2026-11-12):** before building a capability, look for it inside Susi, then in Apache-2.0 projects, then in the locked dependency graph; reuse by harvesting only the needed part under Mandate 75's pure-`Apache-2.0` rules; delete the hand-rolled code a reuse replaces; stay inside the lean ceilings, the `<10s` budget, Rust-only and the dependency ceilings; record the decision in the task record's `reuse` object ([`docs/ceo-mandate-78.md`](docs/ceo-mandate-78.md)). |

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

## Open-core boundary

Susi core is the free, Apache-2.0, open-source socket. It is the standard-protocol
AI OS for one host: MCP, ACP (Agent Client Protocol / Agent Communication
Protocol), A2A, OpenAI-compatible HTTP, Anthropic Messages, extension admission,
and the Wasm host those plugs run in, built around **five primary core pillars**
on `susi-core`: **GAWD** (`susi-gawd*`), **GEMI** (`susi-gemi*`), **GMCP**
(`susi-gmcp`), **ACP** (`susi-core::acp` open socket), and **Susi Harness**
(`susi-harness`, with built-in parallel multi-agent workflow). It stays lean and
blazing fast: `<10s` (`total_ms < 10_000`) warm core build+test+release pipeline
budget (Mandates 69 and 74), measured and enforced rather than asserted.

Core does not ship a third-party product implementation. A third party writes the
plug, and core admits it over the socket. Do not add an implementation of a named
product (an agent framework, a model host, a cloud CLI, a browser, a catalog, or
a search vendor) to this repository. Speaking a standard protocol is the socket.
Embedding that vendor's product is not. The crates `susi-vendor-agents` (≤ 9 files),
`susi-vendor-models` (≤ 37 files), `susi-vendor-cloud` (≤ 1 file),
`susi-vendor-chrome`, `susi-vendor-web` and `susi-vendor-candle` are migration
debt: their Rust source may only shrink.
`third_party_product_implementations_only_shrink` holds the ceiling.

Susi Commercial Apps (`intellibitz/susi-commercial-apps`) is the closed-source,
proprietary, cryptographically licensed (`SUSI-ENT-v1` / Ed25519 `SUSI-CUST-v1`),
and paid software suite built for commercial entities, corporates and CEOs across
the local and cloud AI ecosystem. Every add-on feature, UI, desktop/operator
workspace, enterprise ACP agent bridge, multi-user administration, SSO/RBAC,
fleet management across hosts, central FinOps spend governance, executive
dashboards and turnkey corporate packs lives exclusively there, admitted over
this public socket. Never commit proprietary code, licensing keys or non-public
product plans here, and never move commercial code into this repository or the
reverse. Never admit a dependency whose license would bind software built on top
of Susi. The Apache-2.0 license grants code rights, not rights to the Susi name
or marks.

## Lean-core program (CEO directive 2026-11-07, updated 2026-11-11 Mandate 74)

Susi core becomes minimal and blazing fast (`<10s` warm build+test+release
pipeline budget; `<60s` cold compile budget): **GAWD, GEMI, GMCP, ACP and the
Susi harness** (with its built-in parallel multi-agent workflow) are the only
primary components; everything else is an add-on plug that lives exclusively in
`susi-commercial-apps` and is admitted over the socket. The plan, the crate
classification, the measured ladder toward zero third-party dependencies and the
split of work between agents are in `docs/lean-core.md`. The ceilings in
`tests/lean_core_ceilings.rs` only go down: every cut lowers them in the same
change, and nothing raises one without a CEO decision recorded in the doc.
Before migrating a module out of core, grep every cfg (`#[cfg(windows)]` code
is invisible to a Linux build), then `cargo check --workspace --all-targets`
and `cargo fmt --all --check` on the exact tree, and ship through the gate.

## Native local agent loop

Every workflow step below (`start`, `sync`, `commit`, `distill`, `ship`,
`release`, and parallel Susi Harness worker steps) supports **remote pull and
merge reconciliation** against `origin` over pure Git transport whenever
reachable (falling back cleanly to offline local Git operation when
disconnected, Mandate 66).

1. Inspect the actual repository; fetch and merge `origin/main` before you
   start, when you resume and before every commit (Mandate 74: every agent
   works on the latest remote; `susi-commercial workflow fresh` reports the
   lag), and preserve others' work. Do not work in the primary checkout or on `main`.
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
   Releases are distilled from these labels (docs/distillation.md): every
   commit carries `Task: <id>`, every task record declares its `scopes`, and
   the record is closed when its work lands. Check your range before
   publishing with `cargo run --offline --locked -p xtask --bin distill --
   lint --range origin/main..HEAD --strict` (fast, advisory, not a main gate).
   Never disable shared hooks globally. Legacy network hooks are not the local
   correctness gate; use the native command instead of handing work to GitHub.
6. Serialize integration and publication; reconcile the current main head and
   preserve all accepted work. Main is explicitly unqualified development and
   has no mandatory pre-push gates under Mandate 73. Use normal atomic
   fast-forward updates, never force-push or reset shared history. Verify the
   target origin contains the commit before reporting it landed. Ordinary
   development checks remain useful but cannot become a hidden main gate.
   Existing `cargo local-workflow ship <name> [<name> ...]` still runs its older
   local gate before publishing; that is implementation drift to migrate.
   Authorized normal Git publication may carry a main change in the meantime,
   without modifying hooks globally, authentication, rulesets or branch access.
   Full exact-tree tests/audits/provenance gates belong on release branches and
   snapshots. Do not install or claim a main artifact as qualified production.
   Releases remain immutable tags plus committed records (`releases/<tag>.json`,
   `docs/releases.md`); no GitHub Actions or hosted-workflow prerequisites.
7. Close only after acceptance and integrated-tree verification succeed for the
   complete task. Zero executed tests do not count. Release ownership deliberately;
   `cargo local-workflow release <name>` preserves the branch/worktree and refuses
   dirty state. A partial result or stale receipt never closes a broader mandate.

The native harness must eventually own this whole loop (Mandate 61). Available
commands are building blocks; reservations alone are not enforced filesystem
isolation, and compilation alone is not production qualification. Cross-host
workers need one shared coordinator, not independent local locks.

## Production and self-improvement

The installed Susi is the stable copy (Mandate 48, CEO decision 2026-11-12): it
changes only by promotion of a fully qualified release and is not the tool that
builds its successor. Development is driven by private debug builds from
current `main`, one per agent. **Agents must keep the local release binary
updated with the remote release, whenever feasible and required** (CEO
directive 2026-11-12): required when the installed copy is behind the latest
release qualified for this host; feasible when that release is qualified here,
the remote answers and no susi client is mid-mission. The installed updater
(`susi-release-sync`, hourly where its timer is enabled) builds the release
from its tag and promotes it; `susi-commercial workflow check` measures the
installed copy against the latest qualified release and starts the updater when
it is behind. If the updater fails, fix the cause or report it on the Susi
Board; never leave the installed copy behind silently. Production qualification requires an identified source tree and
locked graph, strict license and Rust-only audits, format/lint checks, nonempty
required tests, executable smoke checks, artifact digest and reproducible
source/toolchain/target/feature evidence. Then atomically promote, retain the
previous verified artifact, check health, and roll back on failure. Test the
promotion/rollback path, including Windows file locks and interruption.

The CEO grants standing authority to **commit, push and merge** Susi work at
agents' discretion. Agents own review and meaningful testing; the
integrator serializes merges and preserves accepted work. Main has no mandatory
pre-push gates and is unqualified development. Full exact-tree verification gates
apply to release branches, release snapshots and production qualification. Do not ask
for routine CEO approval of those Git actions. Preserve other agents' work and
never force-push or bypass release evidence gates or access controls. GitHub publication remains optional
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
    susi-error, susi-gawd, susi-gawd-agents, susi-gmcp,
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
  - `susi-vendor-winapi` retains `deny(unsafe_code)`. Its original detached
    process wrapper permits only justified per-function FFI allowances for
    explicit UTF-16 Windows process creation without handle inheritance,
    ordinal environment comparison, and closing returned handles. No module-wide
    allowances, shell fallback, or process-global handle mutation. Preserve the
    measured unsafe-exposure ratchet and actual argument/pipe-EOF regressions.
    Its `acl` module is the only other first-party allowance: per-function FFI
    for setting and reading a file's owner-only DACL, creating a file with
    that DACL applied in the create call itself (so no handle is ever opened
    against a wider list), and the process-token user SID, so no console
    helper such as `icacls` is launched. The same rules apply (no module-wide
    allowance, `// SAFETY:` evidence, no raw pointer or raw handle out of the
    module); the callers stay `forbid(unsafe_code)`.
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
- **Do not add a third-party product implementation.** A third party writes
  the plug and core admits it (open-core boundary above). The existing
  `susi-vendor-{agents,models,cloud,chrome,web,candle}` crates are migration
  debt and may only shrink. Facades that re-export a library stay. No
  third-party endpoint strings or vendor product logic in non-vendor crates.
- `susi_http_transport::http_call*` returns non-2xx responses; callers must
  check status. `HttpCall::into_bytes(max)` reads `max + 1` and rejects overflow.
  The std-only `susi_paths::loopback` client caps complete responses at 16 MiB.
- Do not redeclare normal dependencies under dev/build scopes unless extra
  test-only or build-only features are required.
- Standalone leaf-service binaries enable only their `service-*` feature;
  root `service-run` may enable `all-services`.
- No stubs: no `todo!()`, `unimplemented!()`, or dead `pub` surfaces left
  "for later".

## Credentials come from KeePassXC

KeePassXC is the password manager for the CEO and for every agent working on
Susi, in this repository and in `intellibitz/susi-commercial-apps` alike. No
credential lives in a repository file, a `.env` file, a shell profile, CI logs,
chat, or agent memory.

- Two vaults. The CEO's **Personal vault** is off limits to agents: never open
  it and never ask for its passphrase. The **automation vault** under
  `%USERPROFILE%\KeePassXC-AppLauncher` on the CEO's workstation is the agent
  credential source; its unlock is bound to the CEO's Windows account, and the
  launcher's `Usage.md` there is the reference. Entries live under
  `App credentials/` (model API keys and `GH_TOKEN` forwarded into agent shells,
  GPG and SSH signing identities, and the commercial release authority key).
- Agents read it only through the launcher's `AgentVault.ps1` helpers
  (`Read-AgentFields`, `Invoke-AgentKdbx`), and a value moves only through a
  pipe or a child process's environment to its consumer, for example a command
  that reads it from stdin. Never print, log, echo, write to a file,
  paste in chat, or pass a value on a command line; report "set" and at most a
  byte length, and clear variables afterwards.
- New credentials are generated in-process, stored in the automation vault,
  read back and compared, then exported. Rotation updates the vault entry
  first, then every consumer, then restarts long-running agents.

## Platform and CI policy (CEO decisions 2026-11-03, 2026-11-05 and 2026-11-06)

- **Susi core (`intellibitz/susi`) is free, open (Apache-2.0 forever), and ships only for vanilla Windows x86_64 and vanilla Linux x86_64** for budget reasons. Do not publish core release or download artifacts for macOS, ARM64 or any other platform. Releases are qualified and built locally; the GitHub Actions release workflows were removed (CEO decision 2026-11-06), and GitHub Actions stays disabled at the repository level (CEO decision 2026-11-05).
- **Zero GitHub Actions; the gate is local and current-host only** (CEO decision 2026-11-05). No hosted CI runs for core. Development verification can use `cargo local-workflow check` in the agent's own worktree; the existing serialized `cargo local-workflow ship` also runs its older gate. Mandate 73 removes mandatory main-push gates and retains full release-branch qualification. Run checks on the current native host (`x86_64-pc-windows-msvc` or `x86_64-unknown-linux-gnu`). All GitHub Actions workflow definitions were removed (CEO decision 2026-11-06); `xtask/tests/ci_workflow_perf.rs` (`no_github_actions_workflow_returns`) keeps `.github/workflows/` from returning. Never add, enable or trigger GitHub Actions, and never add macOS, ARM64 or other-platform compile checks, native tests, nightly runs or release legs unless the CEO explicitly asks.
- **Keep core source portable.** Do not delete code that already works on other platforms (including ARM64 Windows runtime/link-layer source support, `deny.toml` targets, and `tests/zc_matrix.rs` dependency-graph checks that run on x86_64). Portable code costs nothing; building and testing other platforms in core does.
- **Commercial Apps (`intellibitz/susi-commercial-apps`) macOS/ARM64 support is paused** (CEO decision 2026-11-05) until the CEO reopens it: no every-OS release qualification and no Susi Certified macOS/ARM core builds run in either repository meanwhile. If it is reopened, commercial owns that qualification and must never depend on core CI evidence for macOS or ARM64.

Agents must not add macOS, ARM64 or other non-release platform build, test, nightly or release legs in core.

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

`CLAUDE.md` and `GEMINI.md` point here without
copying a divergent workflow. `susi_core::self_build::BRIEF` carries the same
contract into delegated tasks. Update them and governance tests together when
requirements change. Legacy scripts and GitHub adapters may remain during
migration, but must not become prerequisites or contradict these mandates.

## Handoff and rescue of unfinished work

Many task copies hold work not yet on GitHub storage, because pushes are refused
when a non-fast-forward update is attempted against `main`. If you own such a
copy: pull and merge `origin/main` (resolve conflicts only inside your own
scoped work); publish unfinished handoffs to the non-branch agent ref
`refs/agents/<agent>` (`docs/agent-refs.md`) or dump completed labelled work to
`main` via fast-forward (never push `refs/heads/wip/*` or open a pull request,
Mandates 73 and 74); never commit secrets or key files, and double-check
certificate or key files first; sign your task-log entry with your agent name
(Codex, Claude, Gemini or Grok) and state what is finished and what is left; do
not delete another agent's copy, and skip any copy another agent is actively
using.

If you hit your weekly limit, leave a note in the task log for the next agent
saying where you stopped.

## Remote and cloud incident safeguards (2026-11-08, updated 2026-11-10)

The 2026-11-08 rules responded to ungated remote pushes, broken main, unproven
commercial migrations and stubbed capabilities. Mandate 73 now supersedes their
fixed push cadence, single-CEO-laptop-only publication role and mandatory main
push gates. It preserves:

- Own authorized identity; no impersonation, shared credentials or bypass.
- Isolated claims and task records with task trailers; preserve other agents' work.
- Serialized integration and normal fast-forward main updates without overwrites.
- Full exact-tree verification for release branches and snapshots, not main pushes.
- Destination-first commercial migration, actual wiring/tests and no error stubs.
- No force-push of shared history, and no modification of access controls.
- Actual origin evidence before claiming landing; honest test commands and counts.

A worker that cannot publish to `main` hands its work off on the non-branch ref
`refs/agents/<agent>` (`docs/agent-refs.md`), never on a `refs/heads/*` branch;
`local-agent/<agent>` is its local branch name only. Existing
`susi_harness::workflow::IntegrationPolicy` branch-only and cadence restrictions
are migration work, not proof that the new workflow is complete and not permission
to bypass a live control. Follow `docs/ceo-agent-mandate.md` and the current-state
notes in `docs/harness-coordination.md`. A security/access change needs separate
explicit authorization; normal verified Git delivery does not.
