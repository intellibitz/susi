# susi-harness

Model routing, metering and budgets for fleets of coding agents.

The harness decides which model runs each task, puts every call through a
metering gateway, and keeps spend inside the money you actually have. It reads
your API keys from the environment, works out what they can do, and builds the
model ladder each time it runs. One provider with one model is enough.

It is part of [susi](../../README.md), but it does not depend on the rest of
susi. The standalone `susi-harness` binary contains the provider registry and
the price catalogue, so it keeps working even when nothing else in susi builds.

## Build

```bash
cargo build --release -p susi-harness --bin susi-harness
```

This produces `target/release/susi-harness` (`susi-harness.exe` on Windows).
Inside a susi checkout, `susi harness <command>` runs the same code.

## Quick start: bring your own key, nothing to configure

```bash
export OPENAI_API_KEY=...   # or any other provider's key below; one is enough
susi-harness
```

That's the whole setup. `susi-harness` with no arguments:

1. Finds your keys. It checks the environment, a `.env` file in the current
   folder, and `~/.susi/.env`.
2. Checks each provider live and reads its balance where the provider
   publishes one.
3. Builds the model ladder and picks an orchestrator.
4. Prints the fleet and a one-line budget report.
5. Starts an OpenAI-compatible gateway on `http://127.0.0.1:9180/v1`.

With no key at all, it lists each provider, the variable its key goes in,
and the page where you can get one.

Point any OpenAI-compatible client at the gateway and pick a model from the
fleet. The gateway sends each call to that model's provider with that
provider's key, prices the call, and appends a row to the ledger before it
returns the answer. The ledger lives in `~/.susi/harness/ledger.jsonl`, so
what the harness learns (spend, latency, failures) follows you from one
folder to the next. In a susi workspace (a checkout with
`deploy/harness/prices.json` or `.agents/roadmap.json`, or any folder that
already has `out/ledger.jsonl`) it uses the workspace's own
`out/ledger.jsonl`, from the first call on.

A provider paced to a rate limit (`SUSI_HARNESS_RPM_<PROVIDER>`,
`SUSI_HARNESS_TPM_<PROVIDER>`, below) holds a call for its slot for at most
the gateway's `--timeout`, and never over 120 s. A call whose slot is further
off is answered 429 at once, with a `Retry-After`; the harness's own worker
waits that out and sends the turn again.

### Providers

| Provider | Key | Models |
|---|---|---|
| OpenAI | `OPENAI_API_KEY` | `gpt-luna` (worker), `gpt-sol` (rescue) |
| Anthropic | `ANTHROPIC_API_KEY` | `claude-haiku`, `claude-sonnet`, `claude-opus` |
| Google Gemini | `GEMINI_API_KEY` or `GOOGLE_API_KEY` | `gemini-flash`, `gemini-pro` |
| DeepSeek | `DEEPSEEK_API_KEY` | `deepseek-flash`, `deepseek-pro` |
| Moonshot (Kimi) | `MOONSHOT_API_KEY` or `KIMI_API_KEY` | `kimi-flash`, `kimi-k3`, `kimi-k3-pro` |
| OpenRouter | `OPENROUTER_API_KEY` | `or-deepseek-flash`, `or-qwen-flash`, `or-gemini-flash`, `or-claude-sonnet`, `or-gpt-sol`, `or-claude-opus` |
| SiliconFlow | `SILICONFLOW_API_KEY` | `siliconflow-deepseek-flash`, `siliconflow-glm-flash`, `siliconflow-qwen`, `siliconflow-qwen-coder` (workers); `siliconflow-deepseek`, `siliconflow-deepseek-pro`, `siliconflow-glm`, `siliconflow-kimi-k3` (rescue) |
| DeepInfra | `DEEPINFRA_API_KEY` | `deepinfra-deepseek-flash`, `deepinfra-qwen-flash`, `deepinfra-glm-flash`, `deepinfra-llama`, `deepinfra-qwen-coder` (workers); `deepinfra-deepseek`, `deepinfra-deepseek-pro`, `deepinfra-kimi-k3` (rescue) |
| DashScope (Qwen) | `DASHSCOPE_API_KEY` | `dashscope-qwen-plus`, `dashscope-qwen-coder`, `dashscope-qwen-max` |

Any one key gives a complete fleet: workers, rescue models and an
orchestrator. A test enforces this for every provider. More keys give a
deeper ladder, and to add a provider you write a registry file (see below).

SiliconFlow and DeepInfra are the two providers that serve the open models
(DeepSeek, Qwen, GLM, Kimi) on one key each, and their models were each
checked with a live tool call when the registry was last refreshed. To make
them the fleet's main routers, name them:

```bash
export SUSI_HARNESS_PREFER=siliconflow,deepinfra
```

Their models then lead every tier, SiliconFlow's first, and SiliconFlow's
cheapest strong model orchestrates; the other keyed providers follow as
fallbacks in the usual price order. The two publish what they serve
differently: DeepInfra's model list carries each model's prices, context
window and capabilities, which `susi-harness models --discover` turns into an
overlay you can review and adopt, and DeepInfra states each call's cost in
its answer, which the ledger records in place of the catalogue's estimate.
SiliconFlow's list is ids alone (the registry knows which query narrows it
to chat models), and its balance endpoint was retired in August 2026 with no
replacement, so its budget is `SUSI_HARNESS_BUDGET_SILICONFLOW_USD` or,
unset, "active while its models endpoint answers".

### As part of susi AI OS

Inside susi, `susi harness <command>` runs the same code with the same
defaults, and susi's own build workflows run through it. On its own,
the `susi-harness` binary needs nothing else from susi. Use it to build
anything, with any agent or tool that speaks the OpenAI API.

## Commands

| Command | What it does |
|---|---|
| `susi-harness` (or `susi-harness up`) | Finds your keys, prints the fleet and budget, and serves the gateway. Takes the gateway flags. |
| `susi-harness fleet [--workers auto\|m1,m2]` | Prints the orchestrator and the escalation ladder. |
| `susi-harness models` | Lists the models your keys make usable, and the ranking evidence. |
| `susi-harness models --verify [--provider ID]` | Checks each provider live: key accepted, models present. |
| `susi-harness models --discover [--provider ID]` | Lists the chat models each keyed provider serves beyond the registry, as an overlay document with the prices the provider publishes; notes go to stderr. |
| `susi-harness budget` | Prints the one-line budget report and any top-up or expiry alert. |
| `susi-harness env` | Lists every variable the harness reads; keys are only marked as set. |
| `susi-harness gateway [--port N] [--host H] [--accounts F]` | Runs the metering gateway. |
| `susi-harness version` | Prints the version. |

`--ledger` and `--catalogue` apply to every command.

- `--ledger` defaults to `SUSI_HARNESS_LEDGER` if set, then
  `out/ledger.jsonl` in a susi workspace (see Quick start), then
  `~/.susi/harness/ledger.jsonl`.
- `--catalogue` defaults to `deploy/harness/prices.json` when that file
  exists, otherwise the built-in catalogue.

## How models are ranked

The ladder is rebuilt from the live environment on every run:

1. **Eligible**: the model's last real call was not refused; the provider
   has a key and an active budget, and still lists the model; and the model
   can call tools. Real calls are the source of truth, whatever a budget or
   balance says:
   - A model whose last call was refused (401, 402, 403, or an empty
     account) is inactive. It stays out until a real call succeeds: once
     the refusal is at least 5 minutes old, the harness sends it one small
     probe call, when a run starts and again before each task of a running
     mission (`susi harness run`), and records the answer in the ledger. If
     it answers, it is active again. A mission keeps a model it left off at
     its start for a refusal in reserve, so that probe reaches it too; a
     model also off for another reason (an inactive provider, one its
     provider no longer lists) waits for the next run. Tasks prepared
     together share one probe of a model and wait for its answer.
   - A probe is sent from where the harness runs, with the keys set there.
     A model whose key is not set there (its calls go through a gateway
     that holds the key) is never probed: after a refusal it stays off
     until the key is set there, or until a `susi harness run` that drives
     that gateway asks it about the model once the refusal is 5 minutes
     old ([docs/harness.md](../../docs/harness.md)).
   - A model whose last call hit a rate limit, a timeout, a 5xx or a
     dropped connection stays eligible. It only moves back for a while (see
     below), so one bad minute never empties a fleet of one model.
   - A call made through an alias (`coder`) or `susi/<model>` counts for the
     model it names.

   A provider is active with at least $1 left. When the remaining balance
   can't be costed, the provider is active if it answers its models endpoint
   with the key. A model that endpoint no longer lists is left off on its
   own, with an alert; the provider's other models keep running.
2. **Expiring credit first**: a provider whose credit expires soon leads the
   ladder with all of its models, soonest expiry first. Its strongest rescue
   model orchestrates. Credit lost unused costs nothing to spend.
3. **Preferred providers next** (`SUSI_HARNESS_PREFER=siliconflow,deepinfra`):
   their models lead each tier in the order named, ahead of cache and price,
   and the first named provider's cheapest priced strong model orchestrates
   unless expiring credit already chose one. A named provider with no key, no
   budget or no working model changes nothing; `fleet` and `models` report
   the ones in effect as `preferred`. An unpriced model never leads on a
   preference alone.
4. **Cache, then price, then capability, then latency**: cached models come
   first, because a task re-sends the same prompt every turn. Cost is the next
   key, priced at each model's cache rate. When prices are within about 10% of
   each other, the rescue tier prefers the more capable model and every tier
   prefers the faster one, measured from the ledger. A model with no price
   comes after every priced model of its tier.
5. **Roles**: the fleet has three.
   - **Orchestrator**: a strong model that plans and drives. It is the
     strongest strong model on expiring credit, else the first preferred
     provider's cheapest strong model, else DeepSeek Pro, else the cheapest
     strong model.
   - **Workers**: light models (`deepseek-flash`, `gemini-flash`,
     `kimi-flash`, `claude-haiku`, …) that do the work. Every task starts
     on one, and tasks are spread across them.
   - **Fixers**: strong models (`deepseek-pro`, `gemini-pro`, `kimi-k3`,
     `kimi-k3-pro`, `claude-sonnet`, `claude-opus`, …) that fix what a worker
     could not.

   When a worker fails, or its work does not pass (which is where a
   hallucination shows up), the task goes straight to the fixers, not to
   another light model. The other light models come last. Expiring credit
   leads within its role, never across: a strong model on expiring credit is
   a fixer, not a first-line worker. `susi-harness fleet` prints the roles
   and, for each worker, the order its tasks escalate in.

While a task runs, the ladder keeps this order and only moves models back:

- A model that has failed most of three or more attempts goes behind the
  healthy ones.
- A provider that just failed cools down. One "out of credit" answer is
  enough; any other failure needs two in a row, and a completed call clears
  it. A cooling provider's models go last.
- A model whose last call hit a rate limit, a timeout, a 5xx or a dropped
  connection also goes last, for 10 minutes or until a call to it completes.
  Nothing but a refusal is dropped, so a fleet of one model still runs.
- If every model on a task's ladder was refused, the mission stops, as it
  does when its budget runs out, and leaves its remaining tasks for the next
  run instead of failing them without a call.
- A prompt too long for one model goes straight to the next model with a
  larger context window.
- A rate limit, an overload (502, 503, 504) or a dropped connection
  retries the same model once, which keeps its prompt
  cache warm. A turn that runs past its own deadline, and rejected work, move
  up the ladder. Two models failing the same way stop the task, because that
  points at the task, not the models.

## Settings

Nothing needs setting except one key. Every key and setting is read from
these places, first match wins:

1. the process environment;
2. on Windows, the user environment, then the machine environment (so a value
   set after a shell started is still seen);
3. `./.env`;
4. `~/.susi/.env`.

`./.env` belongs to whatever folder you run the harness in, which may be a
repository you just cloned. So the two settings that decide where your keys
are sent, `SUSI_HOME` and `SUSI_HARNESS_PROVIDERS`, are never read from it:
set them in the environment, or `SUSI_HARNESS_PROVIDERS` in `~/.susi/.env`.
If `./.env` sets either one, the harness says once that it ignores it.

`.env` files use the usual `NAME=value` form: `#` comments, including one
after a value (`NAME=value  # note`), an optional `export`, and single- or
double-quoted values, with no escape sequences, so a Windows path means what
it says. A file may be UTF-8, with or without a byte-order mark, or UTF-16
with one (what Windows PowerShell 5 writes with `>` or `Out-File`), with LF
or CRLF line ends. A file that is there but cannot be read, or is not text
in one of these encodings, gets a warning that names it, never its contents.
Run `susi-harness env` to see what is set.

| Variable | Meaning |
|---|---|
| Provider keys (see Providers) | Any number of them; each enables its provider. A key the provider refuses (401 or 403) is reported as refused, and that provider's models are left off. |
| `SUSI_HOME` | The data home, for the ledger and `.env`. Default `~/.susi`. Read from the environment only: not from `./.env`, nor from the data home's own `.env`, which is found through it. |
| `SUSI_HARNESS_LEDGER` | The ledger file. |
| `SUSI_HARNESS_BUDGET_<PROVIDER>_USD` | Total dollars credited to a provider that publishes no balance (Gemini, Anthropic). The harness subtracts the spend its ledger records, so this is not the balance the console shows: after a top-up, raise it by the amount added. DeepSeek and Moonshot report their own balance. |
| `SUSI_HARNESS_CREDIT_EXPIRES_<PROVIDER>` | `YYYY-MM-DD` the provider's credit expires. Its models run first until then, and each run alerts in the last 7 days. |
| `SUSI_HARNESS_RPM_<PROVIDER>` | Requests per minute the gateway sends to the provider. Unset means no limit. |
| `SUSI_HARNESS_TPM_<PROVIDER>` | Tokens per minute the gateway lets the provider serve. Unset means no limit. |
| `SUSI_HARNESS_PREFER` | Provider ids, comma-separated, whose models lead their tier and whose first named orchestrates: `siliconflow,deepinfra`. Order matters; unknown or unkeyed ids are ignored. |
| `SUSI_HARNESS_PROVIDERS` | Path to a registry file whose providers and models are added to the built-in ones. A missing or malformed file stops the harness. Read from the environment or `~/.susi/.env`, never `./.env`. A relative path in `~/.susi/.env` is relative to `~/.susi`; one in the environment is relative to the current folder. |
| `ANTHROPIC_WORKSPACE_ID` | Workspace for an Anthropic console user key (process environment only). |

`<PROVIDER>` is the registry id in upper case, with any character other than
a letter or a digit written as `_`: `OPENAI`, `ANTHROPIC`, `GEMINI`,
`DEEPSEEK`, `MOONSHOT`, `OPENROUTER`, `SILICONFLOW`, `DASHSCOPE`, and any
provider an overlay adds (`together-ai` is `TOGETHER_AI`).

## Adding a provider or a model

Write a registry file in the built-in registry's shape and point
`SUSI_HARNESS_PROVIDERS` at it:

```json
{
  "providers": [
    {
      "id": "local",
      "wire": "openai",
      "endpoint": "http://127.0.0.1:8080/v1/chat/completions",
      "models": [
        {"id": "local-coder", "context": 32768, "tools": true, "tier": "worker"}
      ]
    }
  ]
}
```

`key_env` names the variable that holds the provider's key, as one name
(`"key_env": "LOCAL_API_KEY"`) or a list tried in order
(`"key_env": ["LOCAL_API_KEY", "LOCAL_KEY"]`). A provider with no `key_env`
is treated as local: it has no budget to check. Any other `key_env` stops the
harness.

`models_query` names the query that narrows the provider's models endpoint
to its chat models when the list mixes in other kinds
(`"models_query": "type=text&sub_type=chat"` for SiliconFlow); only
`--discover` sends it. `susi-harness models --discover` writes an overlay in
this shape for you, with every chat model a keyed provider lists beyond the
registry: save it, name it in `SUSI_HARNESS_PROVIDERS`, and copy its `prices`
into the catalogue (`deploy/harness/prices.json`, or the file `--catalogue`
names) so the models it adds are priced. It is a proposal to review: it takes
tool calling and streaming as given for a chat model, and a model that
refuses its first real call is taken off the ladder by that call.

An overlay only adds. It may add models to a built-in provider by restating
that provider's `id`, `wire` and `endpoint` unchanged, but it cannot change a
provider or a model that already exists, reuse an existing model id or alias
for a new model, or give an alias a name that is already taken (a model id,
another alias such as `coder`, or a `provider/model` route such as
`deepseek/…`); restating an alias unchanged is fine. The harness stops and
names the one it would redefine. To route a model somewhere else, give it a
new provider id and a new model id.

A new provider may read a key variable that another provider already reads
(`DEEPSEEK_API_KEY`, say) only to send that key to the same scheme and host,
such as another path on `https://api.deepseek.com`. Sending it to any other
host (a regional endpoint, a proxy of yours) is allowed only in an overlay
you name by its full path, or by any path in `~/.susi/.env`. An overlay named
by a relative path in the environment is read from the current folder, which
may not be yours, so it may not; give such a provider a key variable of its
own instead.

A model without a price still runs. It is ranked after every priced model of
its tier, and its tokens are recorded at zero cost, so no budget cap counts
its spend. An account with a spend cap (`--accounts`) is therefore refused
it. To budget it, add a price to the catalogue.

## The ledger

`out/ledger.jsonl` gets one JSON object per line, written with a single
append so that many agents can share it safely. Each row records:

- the mission, task, attempt, model and real provider;
- tokens, with provider cache hits counted separately;
- the price in force and the settled cost, in micro-dollars: zero, marked
  `cached`, for an answer the gateway's own response cache served, also when
  it came through another susi gateway in front, buffered or streamed; and
  the provider's own figure, marked `reported`, when the provider states the
  call's cost in its answer (DeepInfra's `usage.estimated_cost`), so a stale
  catalogue price cannot misstate what that call cost;
- start and end times;
- the outcome: `completed`, `model_failed` (with its class), `accepted` or
  `rejected`.

Failures and verdicts cost nothing, but the ranking learns from them.

A line torn by a crash or a full disk costs only itself where spend is
checked (mission and daily caps, provider budgets) and in the fleet's
evidence: the file is read line by line as bytes, only the torn line is
skipped, and a row the next append glued onto it still counts.

## Security

- Keys are never printed, logged or written to the ledger. Debug output
  redacts them, and the environment report only says whether a key is set.
  A worker's shell commands run without any provider key variable in their
  environment, and their output is masked before a model sees it. The shell
  still runs with your user's rights; it is not a sandbox.
- A provider with a published key form (Anthropic: `sk-ant-`) only gets a
  value of that form. A wrong value in one scope is skipped for the next
  scope, with a warning that names the scope and never the value.
- Without `--accounts`, the gateway serves only on loopback. To listen on any
  other address, pass `--accounts` with a file that lists at least one
  account; every caller then needs a bearer token that names an account.
  `susi-harness gateway`, `susi harness gateway` and `harness-gateway` all
  refuse to start otherwise, and an accounts file with an empty list does
  not count.
- Each account's spend is booked under its own `name`, whatever
  `x-susi-mission` the caller sends. `budget_usd` caps that spend and
  `requests_per_minute` caps its rate; leave either out and that limit is
  off.
- A capped call is admitted only when its worst case fits in what is left:
  the prompt at the cache-miss rate, and at the output rate the request's
  own `max_tokens` or `max_completion_tokens` plus the one retry the gateway
  makes of an answer starved of output tokens, once for each of the `n`
  answers it asks for. A capped call is sent with its limit as `max_tokens`,
  the name every OpenAI-compatible provider reads: a lone
  `max_completion_tokens` is sent under that name, a call that names both is
  sent the larger, and a call that names none is sent the one it was
  admitted against: the gateway's `--max-output-tokens`, else 8192. A limit
  that is not a whole number above zero (`null`, `0`, `50000.0`, `"8"`) is
  taken out of a capped call, and an `n` that is not a whole number is sent
  as the count it was priced at, so the provider is never asked for more
  than was admitted. A provider that refuses `max_tokens` and asks for
  `max_completion_tokens` (OpenAI's reasoning models) is sent the same call
  once more with the same limit under that name, before the caller hears of
  the refusal; the resend waits for a paced slot of its own, and the
  refusal, which billed nothing, writes no row. That worst case
  stays reserved until the call's ledger row is written, even when the
  caller hangs up while the provider is still answering, so calls running
  together cannot spend the same remaining dollar. The prompt is priced
  from its length, so what the provider fetches is priced apart: each image
  by URL at a ceiling above what any routed provider bills for one image,
  while a file or a video by reference, which has no ceiling, is refused
  (send it inline). A capped account cannot call a model that has no price
  in the catalogue.

  ```json
  {"accounts": [
    {"token": "sk-team-a", "name": "team-a", "budget_usd": "20.00", "requests_per_minute": 60}
  ]}
  ```

  Names are printable ASCII. An account with no name is booked as `account-`
  and a short digest of its token, never the token itself. Earlier releases
  booked such an account under its token: those rows still count against
  its cap, so an upgrade does not give it its budget again, but they still
  hold the token, so treat a token used that way as known to anyone who can
  read the ledger.
- On Ctrl-C or SIGTERM, the gateway stops accepting new calls and finishes the
  ones in flight. Every call that reached a provider still gets its ledger row.

## License

Apache-2.0.
