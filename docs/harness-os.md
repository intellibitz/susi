# SUSI harness as an AI OS

Status: target architecture. The "What exists" sections are on `main` today;
the phases are the path from there, not a description of it.

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
- **Native routing.** [`router.rs`](../crates/susi-harness/src/router.rs) can
  reach DeepSeek, SiliconFlow, DashScope, Gemini, Anthropic and OpenRouter
  directly in direct mode — no Python proxy — and falls back to the configured
  upstream for any model it does not know.
- **A native worker.** [`worker.rs`](../crates/susi-harness/src/worker.rs) is an
  in-process tool loop (`view_file`, `write_to_file`, `replace_file_content`,
  `run_command`, `list_dir`), 25 turns, sanitized paths — no Node runtime. The
  CLI still selects it only when told to; phase 0 flips that default.
- **Isolation.** Every task gets a worktree, a claimed task id and a scope; the
  adapter lands it through the repository's own `workflow finish`.
- **A shell.** The cockpit serves a snapshot and an SSE feed, and operator
  commands are validated and logged, then applied at the next task boundary.

## The seams that are missing

### 1. Provider registry

`router.rs` resolves six families by match arm, and anything else goes to the
`--upstream` URL the gateway was started with. Adding a provider means editing
code, there is no capability handshake (a model that cannot call tools is
discovered twenty turns in), and there are no aliases (`--model coder`).

Target: a registry of records

```json
{"id": "deepseek", "wire": "openai", "base_url": "https://api.deepseek.com",
 "key_env": "DEEPSEEK_API_KEY",
 "models": [{"id": "deepseek-chat", "context": 128000, "tools": true,
             "streaming": true, "price_ref": "deepseek-chat"}]}
```

embedded in the binary, overridable by a user file, with a generic
OpenAI-compatible adapter as the fallback. Dispatch consults the capability
record first: a turn that needs tools is refused or degraded before it spends
anything.

### 2. Ingress

The gateway meters calls from our own workers, but it is not a general ingress:
it requires an upstream for unknown models, has no notion of a caller, and does
not list models. That keeps LiteLLM in the critical path.

Target: the same gateway, publicly shaped — bearer key to budget account,
`/v1/models` from the registry, buffered and streamed completions, cache and
ledger unchanged, per-key caps and rate limits, and a direct mode that needs no
proxy at all. Then any client (curl, OpenCode, Cline, a teammate's script) can
point at the harness and be metered, cached and capped.

### 3. Tools

Five tools are compiled into the worker. There is no registry, no MCP, no
approval gate, and no per-tool sandbox profile.

Target: a tool record — name, JSON schema, handler (`native` or `mcp`), policy
(`auto`, `ask`, `deny`), sandbox — plus an MCP client over stdio and HTTP and an
MCP server that exposes susi's own tools. Approvals travel through the control
log, so the cockpit asks and the supervisor applies.

### 4. Agent graph

One task is one agent. There is no agent definition, no message bus, no handoff,
no reviewer.

Target: an agent spec (`role`, `model` alias, `tools`, `budget`, `isolation`,
`max_turns`) and a typed message bus persisted in the event log. The standard
topology is plan → fan out coders into disjoint worktrees → review the diff
against acceptance → integrate. Every node is budgeted; the reviewer is a
different agent with its own scope.

### 5. Plugins

Everything is compiled in. Adding a provider, a tool or an agent means a crate
change and a release.

Target: a `harness.toml` manifest loaded at startup, out-of-process over MCP
first and in-process as WASM components later. A plugin that asks for a
capability the manifest did not declare is refused.

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
| 0 | Finish native defaults (T-5), then retire the LiteLLM/OpenClaw wording in [harness.md](harness.md) | `cargo test -p susi-harness --lib` + doc review | T-ANTIGRAVITYHARNESSORCHESTRATION-5, T-HARNESSOS-1 |
| 1 | Provider registry, capability handshake, generic adapter, aliases | `cargo test -p susi-harness --lib provider_registry` | T-HARNESSOS-2 |
| 2 | Public ingress, per-key budgets, `/v1/models`, direct mode | `cargo test -p susi-harness --lib ingress` | T-HARNESSOS-3 |
| 3 | Tool registry, MCP client/server, approval policy | `cargo test -p susi-harness --lib tool_registry` | T-HARNESSOS-4 |
| 4 | Agent graph: roles, handoffs, reviewer | `cargo test -p susi-harness --lib agent_graph` | T-HARNESSOS-5 |
| 5 | Plugin manifest (MCP first, WASM later) | `cargo test -p susi-harness --lib plugin_manifest` | T-HARNESSOS-6 |
| 6 | Mission control: graph, per-agent spend, approvals, replay | `cargo test -p susi-harness --lib cockpit` | not queued |
| 7 | The proof mission below | e2e check in `susi_gawd::admin::E2E_CHECKS` | not queued |

Phases 6 and 7 are named but not queued: their shape depends on what phase 4
teaches about the message bus and the reviewer.

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
completed work. The cockpit replays the whole thing from the journal.

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
