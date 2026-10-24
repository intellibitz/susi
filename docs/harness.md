# SUSI harness

The harness is SUSI's orchestration layer: it turns a mission into agent turns,
prices every call, bounds what the mission may spend, escalates a failed task to
a stronger model instead of retrying it blindly, and shows the operator what is
happening while it runs. SUSI uses it to build SUSI.

The architecture beyond this chain — a provider registry, a public ingress, a
tool and MCP layer, an agent graph and plugins — is in
[harness-os.md](harness-os.md), together with the phases that get there.

```
                   ┌──────────────────────────────────────────┐
                   │ CEO cockpit (browser / harness desktop)   │
                   │ spend · tasks · workers · controls        │
                   └───────────────┬──────────────────────────┘
                                   │ GET /api/status · POST /api/command
                                   ▼
 author ── plan.json ──► supervisor ── prepare (parallel) ──► adapter
   │                       │                                   │
   │                       │ land (serial)                     │ worktree + claim
   ▼                       ▼                                   ▼
 provider registry ◄── gateway (meter, attribute, price, ledger) ◄── native worker
   │ direct HTTPS          ▲
   │                       │ any OpenAI-compatible client
   ├── deepseek-flash      │ (/v1/models, /v1/chat/completions;
   ├── deepseek-pro        │  --accounts adds per-key budgets)
   ├── gemini-flash        │
   └── gemini-pro          │
```

## The chain

1. **The provider registry** is the model layer: a record per provider
   (DeepSeek, SiliconFlow, DashScope, Gemini, Anthropic, OpenRouter) with its
   endpoint, the environment variable holding its key, and each model's
   capabilities. The router posts to the provider directly — there is no proxy
   in the path. An override document can add a provider without a release, and
   a model that cannot call tools is refused before it is dispatched.
   `susi harness models --verify` GETs each selected provider's models endpoint
   (10 s bound) and reports any configured backend id the provider no longer
   advertises, exiting 1 on drift; `--provider <id>` limits the check, and
   Anthropic — which has no `/models` — is reported as not verifiable.
2. **The gateway** is the control point. It forwards a completion — directly
   when the registry knows the route, or to `--upstream` for anything it does
   not — prices the provider's own usage at the rate in the catalogue, and
   appends a ledger row before answering. An unpriced model is refused —
   unpriced spend cannot be accounted. `--accounts accounts.json` turns it into
   the public ingress: bearer keys, per-key budgets and rate limits, and
   `/v1/models` served from the registry.
3. **The scheduler** runs the tasks of a mission concurrently. Before every
   turn the budget guard must admit its worst-case estimate; after every turn
   the ledger's actual charge is folded back in.
4. **The supervisor** runs a plan against a susi checkout: prepare a worktree,
   add and claim a task, run the agent, verify the acceptance, land through
   `workflow finish`. It persists after every task, so a rerun resumes.
5. **The cockpit** reads the ledger, the event feed, the supervisor state, the
   plan and the control log, and renders one snapshot. It is read-mostly and
   never a second source of truth.

## Model tiers

DeepSeek drives and Gemini delegates. The registry carries exactly the ids the
providers advertise — `deepseek-flash`, `deepseek-pro` (backend
`deepseek-v4-pro`), `gemini-flash`, `gemini-pro` — and the policy above them is
an alias, so rerouting the fleet never touches a plan:

| Alias | Model | Role |
|---|---|---|
| `coder`, `worker`, `flash` | `deepseek-flash` | the default worker turn |
| `planner`, `reasoner`, `pro` | `deepseek-pro` | planning and escalation when flash does not pass |
| `cheap` | `gemini-flash` | the cheap delegate (large context, vision) |
| `delegate`, `reviewer` | `gemini-pro` | second opinion and review |

`susi harness models --verify` checks each registered backend id against the
provider's own list, so an id retired upstream is reported before a call fails.
Prices for all four live in [deploy/harness/prices.json](../deploy/harness/prices.json).

Anthropic keys come in two kinds: a key scoped to a workspace needs nothing
extra, while a console user key (`sk-ant-usr-…`) is accepted but every request
is then refused until it names the workspace. Set
`ANTHROPIC_WORKSPACE_ID=wrkspc_…` and the router sends the
`anthropic-workspace-id` header; leave it unset for a workspace-scoped key and
the request is unchanged. The key can be checked without spending tokens: a
valid key answers a request missing that header with a structured
`invalid_request_error`, an invalid one with `401 authentication_error`.

Two facts about the Anthropic wire shape, both learned from the live API:

- **Sampling parameters are not forwarded.** The current Messages models answer
  `invalid_request_error: "temperature is deprecated for this model"` for
  `temperature`, `top_p` and `top_k` alike, so the OpenAI→Anthropic translation
  drops all three and the model uses its own defaults. A worker turn — which
  always pins `temperature: 0.0` — otherwise fails before it starts.
- **Tool calling crosses the wire.** OpenAI `tools` become Messages `tools`
  (`function.parameters` → `input_schema`), an assistant `tool_calls` array
  becomes `tool_use` blocks, a `tool` result becomes a `tool_result` block in
  the user turn that answers it (parallel results share one turn), and a
  `tool_use` answer comes back as OpenAI `tool_calls` with
  `finish_reason: tool_calls`. A Claude worker turn therefore runs the same
  tool loop as DeepSeek or Gemini — verified end to end by writing a file.

In `--upstream direct` mode the gateway routes each model through the registry,
so **each provider resolves its own key env**; `--api-key-env` pins one key as
an explicit override (needed only for a single explicit upstream, where it
defaults to `DEEPSEEK_API_KEY`). Before this, a direct-mode gateway without
`--api-key-env` sent the DeepSeek key to every provider, which Anthropic
answered with `invalid x-api-key`.

## Budget

Spend is bounded before an attempt, not discovered after the bill.

- `Budget::estimate` prices the prompt at the cache-miss rate and the whole
  output allowance at the output rate. A cache hit or a short answer only makes
  the real cost smaller, so an admission decided on the estimate is never too
  generous.
- `BudgetGuard` keeps in-memory reservations for the attempts in flight, so
  several tasks can run concurrently without each believing the same remaining
  dollar is free. After every turn the guard takes the ledger's number, which
  is the authority.
- A refused attempt is recorded as `budget_exhausted` and the task is left for a
  later run or a top-up. It is not retried into the wall.
- The cockpit warns at four fifths of the cap and the panel's bar turns amber,
  then red at nine tenths.

A turn that makes calls but never writes is not progress either. The native
worker is bounded twice, in process: `executor::MAX_TURNS` caps how many tool
turns one run may take, and the per-turn deadline (`--timeout`, default 900 s)
stops a run that would outlive the mission. A run that hits either bound is
recorded as `timed out` or `failed`, which the ladder escalates like any other
failure — no subprocess and no guard thread is involved.

A mission budget needs a price catalogue: `susi harness run --budget-usd 0.50
--catalogue deploy/harness/prices.json ...`.

## Cost-ranked escalation

Candidates are not tried in the order they were typed. `rank_models` orders them
by **expected cost per accepted task**:

```
expected = (served calls / accepted tasks) × (average price of an attempt)
```

The evidence is the append-only ledger, so the ranking improves as missions run.
A model with no history is ranked at list price and gets its chance. A model
that is cheap but never succeeds is pushed behind every model that does.

The ladder then decides how many attempts a failure deserves:

| Failure | Next |
|---|---|
| transient provider error (429, overload, connection) | one retry on the same model |
| timeout | escalate immediately |
| semantic failure (work produced but not accepted) | escalate immediately |
| executor error (I/O) | stop; a different model hits the same wall |
| the same failure on a **different** model | stop; the task or the environment is the problem |

The ladder is capped (`Policy::max_attempts`, default 4). A rejected attempt's
worktree is reset to the task record before the next model runs, so a rejected
attempt is never published and never leaks into the next one.

## Cockpit

`susi harness cockpit` serves the panel and the API on loopback:

```sh
susi harness cockpit \
  --ledger out/ledger.jsonl \
  --events out/events.jsonl \
  --plan plan.json \
  --state supervisor-state.json \
  --controls out/control.jsonl \
  --catalogue deploy/harness/prices.json \
  --budget-usd 0.50 \
  --port 9181
```

- `GET /` — the panel (add `?embed=1` for a frameless embed).
- `GET /api/status` — one JSON snapshot: spend, budget, missions, workers,
  decisions, warnings, controls, recent events, the agent graph (`graph`),
  spend per graph role in the focused mission (`role_spend`), and that
  mission's replay timeline (`replay`).
  The graph is read from `--graph` (the cockpit's `graph_path`), which takes
  precedence over `SUSI_HARNESS_GRAPH`, or from `graph.jsonl` beside the ledger
  when neither names a bus.
- `GET /api/events` — the same snapshot once a second over SSE.
- `POST /api/command` — `{"command":"pause","mission_id":"…"}`, `resume`,
  `abort`, `top_up`, `redirect`, `decide`. The command is validated, appended to
  the control log and answered. The supervisor reads the log before each batch
  and stops starting new work when the operator paused or aborted.

The panel has no build step and no external assets, so any harness desktop can
embed it in an iframe. This repository ships a DeepSeek Harness plugin at
`deploy/dsh-cockpit` that registers **two** cockpit entries, because the two are
different surfaces and both belong in the sidebar:

| Sidebar entry | Embeds | URL default |
|---|---|---|
| CEO cockpit (external) | the standalone orchestration cockpit app — the former litellm/openclaw cockpit, split into its own application | `http://localhost:8080/` |
| CEO cockpit (susi native) | susi's own panel, built entirely in-house and served by `susi harness cockpit` | `http://127.0.0.1:9181/?embed=1` |

Each panel keeps its own URL in the browser, so repointing one never moves the
other; the native panel also adopts a URL saved under the pre-split
`susi.cockpit.url` key, so an operator's custom cockpit address survives the
split. Install the plugin with the Harness plugin manager (`install_bundle`,
target `deploy/dsh-cockpit`).

## SUSI building SUSI

```sh
# 1. The gateway, routing directly (provider keys from the environment)
harness-gateway --port 9188 --catalogue deploy/harness/prices.json \
  --ledger out/ledger.jsonl --upstream direct

# 2. The plan: a DeepSeek supervisor authors work for a DeepSeek and a Gemini worker
susi harness author --gateway http://127.0.0.1:9188 --model deepseek-flash \
  --roadmap .agents/roadmap.json VC-203-001 --workers deepseek-flash,gemini-flash --out plan.json

# 3. The mission: one worker model first, escalate on failure, stop at 50 cents
susi harness run plan.json --repo . --gateway http://127.0.0.1:9188 \
  --model deepseek-flash --workers gemini-flash --catalogue deploy/harness/prices.json \
  --budget-usd 0.50 --ledger out/ledger.jsonl --parallel 2

# 4. Watch it
susi harness cockpit --ledger out/ledger.jsonl --plan plan.json \
  --controls out/control.jsonl --catalogue deploy/harness/prices.json \
  --budget-usd 0.50 --port 9181
```

Every task is a real task in the queue: its own worktree, its own claim, its own
acceptance, its own pull request. The harness does not bypass the repository's
rules; it makes them mechanical.

## Draining the roadmap queue

`susi harness plan` turns the queue into a mission that **claims existing
tasks** instead of minting new ones:

```sh
susi harness plan --repo . --limit 2 --mission roadmap-batch --out plan.json
susi harness run plan.json ... --budget-usd 1.00
```

Each plan entry names a task id. The adapter creates the worktree, syncs it,
claims that id with the scopes the task declares, runs its acceptance, commits
the worker's change, and lands it through `workflow finish` — the task record is
already on the branch, so nothing is re-minted. Selection is dependency-ready
only, P0 before P1, smaller first, and **scope-disjoint** within a batch: two
workers editing the same tree would only conflict at merge. `--roadmap VC-…`
narrows a batch to one vector.

## Self-build scorecard

The RSI scorecard reads phases git can prove and lifecycle facts git cannot.
The harness writes the second kind to `.agents/rsi-loop.jsonl`:

```sh
susi harness run plan.json ... --rsi-log .agents/rsi-loop.jsonl
susi harness scorecard --events .agents/rsi-loop.jsonl [--task T-…] [--json]
```

- `implemented` when the worker's change is committed, `gated` when the
  acceptance passed, or `defect` with the outcome and reason when it did not.
- `merged`, with the task's spend in USD from the ledger, only after
  `workflow finish` returns and the close is published — not when the branch is
  merely pushed.
- `released` and `promoted` are deliberately never fabricated. They stay
  missing in the report until a release actually carries the task, which is
  what the scorecard's first-missing-phase display is for.

`susi harness scorecard` is the production caller for
`scripts/check-rsi-scorecard.py`, so measuring a loop no longer depends on a
human remembering a script.

## Where the money goes

The ledger is the answer. One JSON line per priced call with the mission, task,
attempt, model, tokens (including cache hits), the price in force, the cost, and
whether the gateway served it from cache. A later catalogue change cannot rewrite
history, and the cockpit's totals are an integer sum of that file.
