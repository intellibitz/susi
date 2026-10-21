# SUSI harness

The harness is SUSI's orchestration layer: it turns a mission into agent turns,
prices every call, bounds what the mission may spend, escalates a failed task to
a stronger model instead of retrying it blindly, and shows the operator what is
happening while it runs. SUSI uses it to build SUSI.

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
 LiteLLM  ◄── gateway (meter, attribute, price, ledger) ◄── OpenClaw worker
   │
   ├── deepseek-chat        (supervisor / worker)
   ├── deepseek-reasoner    (escalation)
   └── gemini-flash         (worker)
```

## The chain

1. **LiteLLM** is the provider layer: one OpenAI-compatible endpoint over
   DeepSeek, Gemini, SiliconFlow and DashScope, with a response cache. Keys are
   read from the environment; a model whose key is unset fails only when called.
2. **The gateway** is the control point. It forwards a completion, prices the
   provider's own usage at the rate in the catalogue, and appends a ledger row
   before answering. An unpriced model is refused — unpriced spend cannot be
   accounted.
3. **The scheduler** runs the tasks of a mission concurrently. Before every
   turn the budget guard must admit its worst-case estimate; after every turn
   the ledger's actual charge is folded back in.
4. **The supervisor** runs a plan against a susi checkout: prepare a worktree,
   add and claim a task, run the agent, verify the acceptance, land through
   `workflow finish`. It persists after every task, so a rerun resumes.
5. **The cockpit** reads the ledger, the event feed, the supervisor state, the
   plan and the control log, and renders one snapshot. It is read-mostly and
   never a second source of truth.

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

A mission budget needs a price catalogue: `susi harness run --budget-usd 0.50
--catalogue deploy/litellm/prices.json ...`.

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
| executor error (spawn, I/O) | stop; a different model hits the same wall |
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
  --catalogue deploy/litellm/prices.json \
  --budget-usd 0.50 \
  --port 9181
```

- `GET /` — the panel (add `?embed=1` for a frameless embed).
- `GET /api/status` — one JSON snapshot: spend, budget, missions, workers,
  decisions, warnings, controls, recent events.
- `GET /api/events` — the same snapshot once a second over SSE.
- `POST /api/command` — `{"command":"pause","mission_id":"…"}`, `resume`,
  `abort`, `top_up`, `redirect`, `decide`. The command is validated, appended to
  the control log and answered. The supervisor reads the log before each batch
  and stops starting new work when the operator paused or aborted.

The panel has no build step and no external assets, so any harness desktop can
embed it in an iframe. This repository ships a DeepSeek Harness plugin at
`deploy/dsh-cockpit`: it registers a **CEO cockpit** entry in the sidebar and a
main panel that embeds `http://127.0.0.1:9181/?embed=1`. Install it with the
Harness plugin manager (`install_bundle`, target `deploy/dsh-cockpit`); the URL
is editable in the panel and stored in the browser.

## SUSI building SUSI

```sh
# 1. The proxy (keys from the environment)
susi harness litellm --config deploy/litellm/config.yaml --port 4000

# 2. The meter
harness-gateway --port 9188 --catalogue deploy/litellm/prices.json \
  --ledger out/ledger.jsonl --upstream http://127.0.0.1:4000/v1/chat/completions \
  --provider openai --api-key-env LITELLM_MASTER_KEY

# 3. The plan: a DeepSeek supervisor authors work for a DeepSeek and a Gemini worker
susi harness author --gateway http://127.0.0.1:9188 --model deepseek-chat \
  --roadmap .agents/roadmap.json VC-203-001 --workers deepseek-chat,gemini-flash --out plan.json

# 4. The mission: one worker model first, escalate on failure, stop at 50 cents
susi harness run plan.json --repo . --gateway http://127.0.0.1:9188 \
  --model deepseek-chat --workers gemini-flash --catalogue deploy/litellm/prices.json \
  --budget-usd 0.50 --ledger out/ledger.jsonl --parallel 2

# 5. Watch it
susi harness cockpit --ledger out/ledger.jsonl --plan plan.json \
  --controls out/control.jsonl --catalogue deploy/litellm/prices.json \
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
whether the proxy served it from cache. A later catalogue change cannot rewrite
history, and the cockpit's totals are an integer sum of that file.
