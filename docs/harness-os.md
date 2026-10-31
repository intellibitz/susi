# SUSI harness as an AI OS

Status: phases 1–6 have landed on `main`; phase 7 is the path from there.
"Delivered" below means the code and its tests are merged, not that the OS is
complete.

An agent operating system is not a chat window. It is a runtime that schedules
work, admits spend before spending it, isolates agents from each other and from
the host, and journals every action so a mission can be replayed, audited and
learned from. SUSI already has that kernel. What it does not have yet are the
five seams that let anyone plug in a model, a tool, an agent or a UI without
recompiling it.

## The kernel model

| OS concept | In `susi-harness` | Job |
|---|---|---|
| Kernel | [`supervisor`](../crates/susi-harness/src/supervisor.rs), [`scheduler`](../crates/susi-harness/src/scheduler.rs), [`lifecycle`](../crates/susi-harness/src/lifecycle.rs) | Run lifecycle, durable resume, concurrency, escalation |
| Resource manager | [`budget`](../crates/susi-harness/src/budget.rs), [`money`](../crates/susi-harness/src/money.rs), [`ledger`](../crates/susi-harness/src/ledger.rs), [`policy`](../crates/susi-harness/src/policy.rs) | Admission before dispatch; cost per accepted task |
| Drivers | [`router`](../crates/susi-harness/src/router.rs) | Model I/O over provider endpoints |
| Syscalls | [`worker`](../crates/susi-harness/src/worker.rs) | Tools, filesystem, process, verification |
| Processes | one agent turn per task ([`executor`](../crates/susi-harness/src/executor.rs)) | Isolated, budgeted unit of work |
| IPC | [`event`](../crates/susi-harness/src/event.rs), [`control`](../crates/susi-harness/src/control.rs) | Typed messages, operator commands, journal |
| Filesystem | [`adapter`](../crates/susi-harness/src/adapter.rs) | Worktree, claim, acceptance, finish |
| Shell | [`cockpit`](../crates/susi-harness/src/cockpit.rs), [`api`](../crates/susi-harness/src/api.rs), `panel.html` | Status, control, spend, replay |

Four invariants hold the model together:

1. **The kernel names no provider and no tool.** Anything it can name is a
   driver or a syscall, and those are data.
2. **Authority is a capability, never ambient.** An agent gets a scope, a
   budget, a tool set and a model — nothing else.
3. **Spend is admitted before dispatch.** A call that cannot be priced or
   cannot fit the cap is refused, not discovered on the bill.
4. **The journal is the only history.** Ledger + events + control log are
   append-only; the cockpit is a view, never a second source of truth.

## What exists

- **Durable missions.** The supervisor persists after every task and resumes
  instead of restarting; the scheduler runs tasks concurrently and escalates a
  semantic failure to another model instead of retrying it.
- **Accounting.** Every call carries the price in force when it ran; the budget
  guard reserves worst-case cost for in-flight attempts, and ranking is by
  expected cost per *accepted* task, learned from the ledger.
- **A metering gateway.** Buffered and streamed completions are priced from the
  provider's own usage (cache hits included) before the answer is returned.
- **Native routing.** [`router.rs`](../crates/susi-harness/src/router.rs) reaches
  DeepSeek, SiliconFlow, DeepInfra, DashScope, Gemini, Moonshot, Anthropic,
  OpenAI and OpenRouter directly — no Python proxy — and falls back to the
  configured upstream for any model it does not know. `SUSI_HARNESS_PREFER`
  names the providers that lead the ladder (SiliconFlow and DeepInfra for the
  open models); `models --discover` reads what a provider serves beyond the
  registry, with the prices it publishes, as an overlay to adopt; and a
  provider that states a call's cost (DeepInfra) has that figure, not the
  catalogue's, on the ledger row.
- **A native worker.** [`worker.rs`](../crates/susi-harness/src/worker.rs) is an
  in-process tool loop (`view_file`, `write_to_file`, `replace_file_content`,
  `run_command`, `list_dir`), 25 turns, sanitized paths. It is the only
  executor: there is no external agent runtime to start or select.
- **Isolation.** Every task gets a worktree, a claimed task id and a scope; the
  adapter lands it through the repository's own `workflow finish`.
- **A shell.** The cockpit serves a snapshot and an SSE feed, and operator
  commands are validated and logged, then applied at the next task boundary.

## Components

The harness is one binary and one library; every capability below has exactly
one owner, and `susi harness models` prints the provider half as JSON without a
network call:

| Capability | Owner |
|---|---|
| Model list, provider keys, aliases | [`providers.rs`](../crates/susi-harness/src/providers.rs) — registry records, capability handshake, `coder`/`cheap`/`planner`/`reasoner` aliases |
| Provider routing | [`router.rs`](../crates/susi-harness/src/router.rs) — direct HTTPS per wire, explicit upstream override, `direct` mode |
| Auth, per-key budgets, `/v1/models` | [`gateway.rs`](../crates/susi-harness/src/gateway.rs) + [`ingress.rs`](../crates/susi-harness/src/ingress.rs) — bearer accounts, admission, OpenAI-compatible list, `402`/`401` before dispatch |
| Response cache | [`cache.rs`](../crates/susi-harness/src/cache.rs) — in-memory and on-disk SHA-256 cache; a hit writes a zero-cost ledger row |
| Spend caps and routing policy | [`budget.rs`](../crates/susi-harness/src/budget.rs) + [`policy.rs`](../crates/susi-harness/src/policy.rs) — worst-case reservation, per-mission caps, escalation by expected cost per accepted task |
| The worker loop | [`worker.rs`](../crates/susi-harness/src/worker.rs) + [`agents.rs`](../crates/susi-harness/src/agents.rs) — in-process tool loop and agent graph |
| MCP tools | [`mcp.rs`](../crates/susi-harness/src/mcp.rs) + [`tools.rs`](../crates/susi-harness/src/tools.rs) — stdio/HTTP client, tool records, approval policy |
| Plugin manifests | [`plugins.rs`](../crates/susi-harness/src/plugins.rs) — `harness.plugins.json`, capability-scoped records |

## The seams that were missing

These closed the gap between "a gateway that meters" and "an orchestration layer
that can be handed work": a provider registry, a public ingress, a tool and MCP
layer, an agent graph, and a plugin manifest.

### 1. Provider registry

`router.rs` resolved six families by match arm, and anything else went to the
`--upstream` URL the gateway was started with. Adding a provider meant editing
code, there was no capability handshake (a model that could not call tools was
discovered twenty turns in), and there were no aliases (`--model coder`).

Now: a registry of records

```json
{"id": "deepseek", "wire": "openai", "endpoint": "https://api.deepseek.com/chat/completions",
 "key_env": "DEEPSEEK_API_KEY",
 "models": [{"id": "deepseek-flash", "context": 128000, "tools": true,
             "streaming": true, "price_ref": "deepseek-flash"}]}
```

embedded in the binary, overridable by a user file, with a generic
OpenAI-compatible adapter as the fallback. Dispatch consults the capability
record first: a turn that needs tools is refused or degraded before it spends
anything.

### 2. Ingress

The gateway meters calls from our own workers, but it was not a general ingress:
it required an upstream for unknown models, had no notion of a caller, and did
not list models.

Now: the same gateway, publicly shaped — bearer key to budget account,
`/v1/models` from the registry, buffered and streamed completions, cache and
ledger unchanged, per-key caps and rate limits, and a direct mode that needs no
proxy at all. Then any client (curl, OpenCode, Cline, a teammate's script) can
point at the harness and be metered, cached and capped.

### 3. Tools

The worker had five tools compiled in with no registry, no MCP, no approval gate
and no per-tool sandbox profile.

Now: a tool record — name, JSON schema, handler (`native` or `mcp`), policy
(`auto`, `ask`, `deny`), sandbox — plus an MCP client over stdio and HTTP and an
MCP server that exposes susi's own tools. Approvals travel through the control
log, so the cockpit asks and the supervisor applies.

### 4. Agent graph

One task was one agent: no agent definition, no message bus, no handoff, no
reviewer.

Now: an agent spec (`role`, `model` alias, `tools`, `budget`, `isolation`,
`max_turns`) and a typed message bus persisted in the event log. The standard
topology is plan → fan out coders into disjoint worktrees → review the diff
against acceptance → integrate. Every node is budgeted; the reviewer is a
different agent with its own scope.

### 5. Plugins

Everything is compiled in: adding a provider, a tool or an agent means a crate
change and a release.

Now: a manifest loaded at startup, out-of-process over MCP first and in-process
as WASM components later. A plugin that asks for a capability the manifest did
not declare is refused.

## Capability-scoped agents

The security rule is the same one the repository already uses for work: an
agent holds a claim, and a claim holds scopes. Extend that to runtime —

```
agent = (scope, budget, tool set, model)
```

- the filesystem it may write is its worktree and the paths in its scope;
- the tools it may call are listed, and a denied tool is absent, not refused
  after the fact;
- the money it may spend is a cap, enforced by the budget guard;
- the model it gets is an alias resolved through the registry, so a policy
  change reroutes every agent without touching the agent definition.

No ambient authority: no inherited environment, no home directory, no network
beyond the declared tools. A reviewer is just an agent with read-only paths and
its own budget.

## Phases

| Phase | Deliverable | Gate | Queued as |
|---|---|---|---|
| 0 | ✅ Native defaults: one executor, no external proxy, no external agent runtime | `cargo test -p susi-harness --lib` | T-ANTIGRAVITYHARNESSORCHESTRATION-5, T-HARNESSOS-1 |
| 1 | ✅ Provider registry, capability handshake, generic adapter, aliases | `cargo test -p susi-harness --lib provider_registry` | T-HARNESSOS-2 |
| 2 | ✅ Public ingress, per-key budgets, `/v1/models`, direct mode | `cargo test -p susi-harness --lib ingress` | T-HARNESSOS-3 |
| 3 | ✅ Tool registry, MCP client, approval policy | `cargo test -p susi-harness --lib tool_registry` | T-HARNESSOS-4 |
| 4 | ✅ Agent graph: roles, handoffs, reviewer | `cargo test -p susi-harness --lib agent_graph` | T-HARNESSOS-5 |
| 5 | ✅ Plugin manifest, JSON today (MCP first, WASM later) | `cargo test -p susi-harness --lib plugin_manifest` | T-HARNESSOS-6 |
| 6 | ✅ Mission control: graph, per-agent spend, approvals, replay | `cargo test -p susi-harness --lib cockpit` | T-HARNESSOS-10 |
| 7 | ✅ Live agent-graph missions: `harness-mission` runs planner, coders and reviewer as real turns (reviewer acceptance is "the run succeeded" until a verdict protocol exists) | proof mission below still needs provider keys and worktrees | T-HARNESSOS-12 |

The manifest is `harness.plugins.json` rather than TOML: the tree carries no
TOML parser, and the data format here is JSON. See
[plugins.rs](../crates/susi-harness/src/plugins.rs).

Phase 7 is wired: a mission with an `agents` block and no `dry_run` runs the
graph for real — each role is one native worker turn in its own
`<state_dir>/<role>` workspace, and the handoffs are persisted to
`<state_dir>/graph.jsonl`. The reviewer's `accepted` currently means "its run
succeeded": no verdict protocol exists on the wire yet, so a reviewer that
finishes approves the work. The proof mission below still needs provider keys
and per-agent worktrees.

## The proof mission

One command:

```sh
susi harness mission --roadmap VC-201-0NN --budget-usd 2 --workers 3
```

Three agents in three worktrees, each with its own claim and scope; a planner
that turns a roadmap vector into a validated plan; coders that run tools and
the repository's acceptance; a reviewer that gates each diff; merges that land
through the normal pull request path; a ledger total under the cap. `kill -9`
mid-run, restart, and it resumes without double-spending or re-serving
completed work. The cockpit's `replay` reassembles that journal — the event
feed, the ledger rows and the graph handoffs — into one deterministic,
read-only timeline.

When that runs unattended, the harness is an AI OS in the sense that matters:
it can be handed work and trusted with the resources to do it.

## Design rules

- **The kernel never matches on a provider name or a tool name.** If it does,
  that name belongs in data.
- **Two protocols, not five:** OpenAI-compatible for models, MCP for tools.
  Everything else is an adapter at the edge.
- **Models, tools, agents and missions are data.** Registry, manifests and
  plans are JSON/TOML, not code paths.
- **One binary, no runtime dependencies.** No Node, no Python, no Redis in the
  path that executes work.
- **Least privilege by default:** deny tool, empty environment, scoped
  filesystem, per-agent budget.
- **Evidence over intuition:** the ledger and the journal decide ranking,
  escalation and replay.

## Hard parts

- **Wire normalization.** Anthropic tool calls, OpenAI function calls, Gemini
  schemas, structured output and streaming usage differ. Keep it to three wire
  families plus a generic fallback, and test each against recorded fixtures.
- **Accounting under concurrency.** Reservations, cache hits, retries and
  escalation all touch the same budget; the ledger stays the authority and the
  guard only ever over-reserves.
- **Sandboxing on Windows.** Process trees, path escaping and environment
  scrubbing are weaker than on Linux; per-tool profiles have to be honest about
  what they do not contain.
- **Runaway agents.** Turn caps, stall guards and per-agent budgets are not
  optional; a loop that reads forever must be stopped before it spends the
  mission.
- **Plugin security.** A manifest is a capability list, not a promise: deny by
  default, and refuse undeclared access at the host boundary.
- **Scope creep.** The fastest way to lose the kernel is to grow a UI framework
  or a workflow DSL inside it.

## Non-goals

- A general-purpose chat product; the cockpit is a control surface.
- A workflow DSL; plans are data validated against the repository.
- A vector database or memory service in the kernel; memory is a tool.
- Multi-tenant billing; one operator, keys map to budgets.
- Replacing git, worktrees or claims with a private sandbox abstraction.

## See also

- [harness.md](harness.md) — the current chain, end to end.
- [harness-runbook.md](harness-runbook.md) — operating a mission.
- [harness-faq.md](harness-faq.md) — common questions.
