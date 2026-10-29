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
   ├── gemini-pro          │
   └── kimi-flash          │
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
   advertises, exiting 1 on drift; `--provider <id>` limits the check.
   Anthropic's list sits beside its Messages route (`/v1/models`); only a
   provider whose chat endpoint gives no models URL is reported as not
   verifiable.
2. **The gateway** is the control point. It forwards a completion — directly
   when the registry knows the route, or to `--upstream` for anything it does
   not — prices the provider's own usage at the rate in the catalogue, and
   appends a ledger row before answering. A model the catalogue cannot price
   is still forwarded: a working model is never refused for lack of costing.
   Its tokens are recorded at a zero price, so no budget — the mission's
   `--budget-usd`, `--daily-usd` or a provider budget — counts that spend, and
   an account with a spend cap is refused the model outright; add a price to
   budget it. `--accounts accounts.json` turns it
   into the public ingress: bearer keys, per-key budgets and rate limits, and
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

DeepSeek Pro orchestrates; the flash tier works and the pro tier backs it up.
The registry carries exactly the ids the providers advertise — `deepseek-pro`
(backend `deepseek-v4-pro`, the orchestrator), `deepseek-flash`, `gemini-flash`,
`gemini-pro`, `kimi-flash` (backend `kimi-k2.7-code-highspeed`), `kimi-k3-pro`
(backend `kimi-k3.1-pro-highspeed`) — and the policy above them is an alias, so
rerouting the fleet never touches a plan:

| Alias | Model | Role |
|---|---|---|
| `orchestrator`, `primary` | `deepseek-pro` | the orchestrator |
| `coder`, `flash` | `deepseek-flash` | a worker |
| `worker`, `cheap` | `gemini-flash` | the primary worker (large context, vision) |
| `kimi` | `kimi-flash` | a worker |
| `backup`, `backup-worker`, `planner`, `pro` | `deepseek-pro` | backup worker (escalation) |
| `delegate`, `reviewer` | `gemini-pro` | second opinion and backup worker |
| `kimi-pro` | `kimi-k3-pro` | backup worker |

### Automatic ladder from the environment

The ladder is built from the API keys in the environment, so nothing has to be
listed by hand. Each registry model carries a `tier`: `worker` for the light
first attempts and `rescue` for the strong models that fix what a worker could
not. Every built-in provider has both, so any one key gives a complete fleet:

| Provider | Workers | Rescue |
|---|---|---|
| DeepSeek | `deepseek-flash` | `deepseek-pro` |
| Gemini | `gemini-flash` | `gemini-pro` |
| Moonshot (Kimi) | `kimi-flash` | `kimi-k3`, `kimi-k3-pro` |
| Anthropic | `claude-haiku` | `claude-sonnet`, `claude-opus` |
| OpenAI | `gpt-luna` | `gpt-sol` |
| OpenRouter | `or-deepseek-flash`, `or-qwen-flash`, `or-gemini-flash` | `or-claude-sonnet`, `or-gpt-sol`, `or-claude-opus` |
| SiliconFlow | `siliconflow-qwen`, `siliconflow-qwen-coder` | `siliconflow-deepseek` |
| DashScope | `dashscope-qwen-plus`, `dashscope-qwen-coder` | `dashscope-qwen-max` |

Only a model with no `tier` (one an overlay adds without one) is addressable
by name but never chosen automatically.

A model is eligible when its provider's key is set and the provider is active
(at least $1 left, or, when no balance can be read, it answers its models
endpoint with the key), the model can call tools, the provider's model list
still advertises it, and its last real call was not refused (see below). A
model the catalogue cannot price stays on the ladder and is listed under
`unpriced`; it ranks after every priced model of its tier, and its spend is
recorded at zero (see the gateway above).

Within a tier, priced models come first, in this order (the unpriced ones
follow in the same order):

1. **Expiring credit** (`SUSI_HARNESS_CREDIT_EXPIRES_<PROVIDER>`), soonest
   expiry first: credit lost unused costs nothing to spend.
2. **Cache.** A model that serves a cached prefix comes first, because a task
   re-sends the same prompt on every turn.
3. **Price**, by the agent usage profile priced at each model's cache rate,
   in bands about 10% wide.
4. **Capability**, in the rescue tier only: streaming and vision, then the
   larger context.
5. **Measured latency** from the ledger, fastest first.
6. The exact price, then the id.

The **orchestrator** is the most capable rescue model on expiring credit when
there is one, else `deepseek-pro` when it is eligible, else the first-ranked
rescue model, else the first worker.

`--workers auto` is the default for `susi harness plan`, `author` and `run`.
A task starts on a worker; when it fails or its work does not pass, it goes to
the rescue models, then to the other workers. `--workers` with a
comma-separated list still names the ladder explicitly; a listed model whose
provider has no key set or is inactive, whose last real call was refused, or
that its provider no longer lists, is left off and named.

"No key set" is about the command's own environment, and the gateway may hold
keys the command does not: one on an explicit `--upstream` (a proxy with the
keys), or one started with `--api-key-env` or in another environment. So
`run` and `author`, and `plan` when given `--gateway`, ask the gateway they
drive about such a model with one small call through it. The model stays when
the gateway answers, a rate limit or a 5xx included, and is left off when the
gateway cannot route it either (a direct gateway missing the same key answers
that at once, without a provider call) or the provider refuses the call.
`--workers auto` still needs the keys where the command runs.

Such a model is never probed from the command (a probe is sent from where the
command runs, with the keys set there; see below), so after a refusal the
gateway is asked again instead: once the refusal is 5 minutes old, the next
`run`, `author` or `plan --gateway` asks it with the same small call, and
keeps the model when the gateway answers. A command that drives no gateway
(`plan` without `--gateway`, or the standalone `susi-harness`) leaves the
model off until its key is set where that command runs.

`susi harness models --available` prints the resolved `auto` section, and
exits with a message when no key is set.

### What a failed call does to a model

The real calls are the source of truth. The gateway writes a `model_failed`
row when a provider refuses the key or has no credit (401, 402, 403, or an
empty-account body), rate-limits (429), times out (408, 504, or a transport
timeout), fails with a 5xx, or drops the connection. A call that gets any
other 4xx (a 400 or 404), or that the gateway cannot route (no key set),
writes no row and leaves the model's record as it was; a probe (below) records
a 400 or 404 as a transient failure. The model's last recorded call decides,
whatever its budget or balance says. Calls made through an alias (`coder`) or
with the `susi/` prefix count for the model they name.

| Last real call | The model |
|---|---|
| completed | active |
| refused: 401, 402, 403 or an empty account (`no_credit`) | inactive: off the fleet and out of every task's order until a probe answers |
| rate limited, timed out, a 5xx, a dropped connection | stays; for 10 minutes each task tries it after the other models. A fleet of one model still runs it |

A refused model is probed with one small call once the refusal is 5 minutes
old: when a run starts, and again before each task of a running mission, so a
key that was fixed or an account that was topped up comes back without a
restart. That includes a model `susi harness run` left off when it started
because its last call was refused: the mission keeps it in reserve, behind
the models it started with, and a task's ladder takes it back once a probe
answers. A model left off for another reason as well (an inactive provider,
or one its provider no longer lists) waits for the next run. A model whose
key is not set where the run executes is not probed at all, at its start or
mid-mission: a refusal keeps it off for the rest of the mission, and the next
run asks its gateway about it, as described above. Tasks prepared at once
that reach the same refused model share one probe: the others wait for its
answer instead of reading the model as still refused.

If every model on a task's ladder has been refused, the mission stops as it
does when its budget runs out, and leaves its remaining tasks for a later run
instead of failing them unattempted. Work its batch had already prepared and
that passed still lands.

A provider whose models endpoint answers is active even when its list leaves
out one of the registry's backends: only that model is left off, with a `⚠`
alert, and the provider's other models keep running.

`susi harness models --verify` checks each registered backend id against the
provider's own list, so an id retired upstream is reported before a call fails.
Prices live in [deploy/harness/prices.json](../deploy/harness/prices.json).

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

### Prompt caching

Caching is on for all three providers, and the meter charges whatever the
provider reports as a cache hit rather than assuming a miss.

| Provider | How it caches | What the harness does |
|---|---|---|
| DeepSeek | automatic, no parameters | nothing — reads `prompt_cache_hit_tokens` |
| Gemini | implicit, above the model's minimum prefix | nothing to request — reads `prompt_tokens_details.cached_tokens` |
| Anthropic | only with explicit `cache_control` breakpoints | the translation marks the system prompt and the last tool with `{"type":"ephemeral"}`, and maps `cache_read_input_tokens` into `prompt_tokens_details.cached_tokens` |

Measured through the gateway on the same ~7k-token prefix, three calls each
(the first is the miss that writes the cache):

| Model | prompt | cached | cost, hit | cost, miss |
|---|---|---|---|---|
| `deepseek-flash` | 7033 | 6784 | 63 µUSD | 759 µUSD |
| `gemini-flash` | 7205 | 5160 | 769 µUSD | 2162 µUSD |
| `claude-haiku` | 9215 | 9203 | 973 µUSD | ~2200 µUSD |

Two honest caveats:

- A prefix below the provider's minimum never caches (Gemini ~2k tokens,
  Anthropic 1024 or 2048 depending on model), so short turns legitimately report
  `hit=0`. That is the provider's floor, not missing wiring.
- Cache *writes* are priced at the miss rate. Anthropic bills writes at 1.25×
  the miss rate, so a turn that only writes a cache is slightly under-charged;
  the read side is exact.

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
--catalogue deploy/harness/prices.json ...`. An attempt on a model the
catalogue cannot price has no estimate to admit and is recorded at zero, so
the cap does not hold it.

## Cost-ranked escalation

`susi harness run` keeps the fleet's order (above) for each task and only
moves models back: an unreliable model, a cooling provider, a model that just
failed transiently. The scheduler (`harness-mission`) ranks a list of
candidates instead; there they are not tried in the order they were typed.
`rank_models` orders them by **expected cost per accepted task**:

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

Every susi workflow runs this way. Mandate 61 (in `.agents/identity.json`,
compiled into the genome, and mirrored in `AGENTS.md`) requires agents to start
work with `susi harness plan` and `run`, not by hand-running `workflow finish`,
`tasks claim` or `tasks close`.

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

A line torn by a crash or a full disk costs only itself where spend is
checked: the mission's `--budget-usd`, the `--daily-usd` cap, the provider
budgets, the scheduler's mission total and the fleet's evidence read the
file line by line as bytes, skip only the torn line, and still count a row
the next append glued onto it. A cut inside a multi-byte character used to
make the whole file read as nothing spent.

When `--ledger` is not given, `run`, `budget` and `gateway` use
`SUSI_HARNESS_LEDGER` if it is set, else `out/ledger.jsonl` in a susi
workspace, else `harness/ledger.jsonl` in the data home (`~/.susi`, or
`SUSI_HOME`).

## Reliability behaviours

A mission is a metered agent in a git worktree. These behaviours decide what it will do when
something goes wrong; each is covered by tests in `crates/susi-harness`.

- **Prescriptive prompt.** A roadmap task whose acceptance names a `gmcp_req_`/`gmcp_vc_` test is
  told to find the sibling test one requirement earlier, append its own test inside the first five
  turns, and only then iterate on the acceptance output. Every other task is told to make its first
  write before reading more than ten files. A 9,000-line test file is otherwise read until the turn
  cap and nothing is written.
- **Acceptance precheck.** After the claim, the task's acceptance runs on the untouched base. If it
  passes on a clean tree the task is already delivered, so the mission closes and publishes it
  instead of spending a ladder on work that can only produce an empty commit.
- **Zero-test refusal.** A `cargo test` acceptance whose output reports zero passed tests is
  refused: a filter that matches nothing exits zero and proves nothing, neither passing a ladder
  nor closing a task.
- **Bounded commands.** Each worker command has a deadline (600 s, `SUSI_TOOL_TIMEOUT_SECS`). On
  expiry the whole process tree is killed and the model is told it timed out, so a test the worker
  wrote cannot wedge the mission.
- **Harness subcommands are refused.** A worker command whose text names `workflow finish|sync|start`
  or `tasks claim|close|release`, or that invokes a `susi`/`susi<N>` binary at command position, is
  refused: claim, close and publish belong to the harness.
- **A failed mission releases its claim.** The worktree stays for diagnosis, but the claim does not
  outlive the attempt, so the next mission can retry without a manual forced release.
- **Stale worktree cleanup.** When `workflow start` refuses because a killed mission left its
  directory or branch behind, the harness removes it and retries once.
- **Shared target.** A new mission worktree links the primary checkout's cargo target, so only the
  workspace crates rebuild instead of the whole registry graph.
- **Merge waits.** A finish publishes its close receipt to the remote and then waits for the merge;
  one accepted-but-unmerged close pipelines, so the next task may be claimed. The waiting process
  holds the clone's workflow lock, so another mission in the same clone cannot prepare until it
  returns or the wait budget (`SUSI_FINISH_WAIT_MAX`, default 30 minutes) expires — plan one mission
  at a time per clone.