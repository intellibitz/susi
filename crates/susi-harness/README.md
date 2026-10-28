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
folder to the next. In a susi workspace it uses the workspace's own
`out/ledger.jsonl`.

### Providers

| Provider | Key | Models |
|---|---|---|
| OpenAI | `OPENAI_API_KEY` | `gpt-luna` (worker), `gpt-sol` (rescue) |
| Anthropic | `ANTHROPIC_API_KEY` | `claude-haiku`, `claude-sonnet`, `claude-opus` |
| Google Gemini | `GEMINI_API_KEY` or `GOOGLE_API_KEY` | `gemini-flash`, `gemini-pro` |
| DeepSeek | `DEEPSEEK_API_KEY` | `deepseek-flash`, `deepseek-pro` |
| Moonshot (Kimi) | `MOONSHOT_API_KEY` or `KIMI_API_KEY` | `kimi-flash`, `kimi-k3`, `kimi-k3-pro` |
| OpenRouter | `OPENROUTER_API_KEY` | `or-deepseek-flash`, `or-qwen-flash`, `or-gemini-flash`, `or-claude-sonnet`, `or-gpt-sol`, `or-claude-opus` |
| SiliconFlow | `SILICONFLOW_API_KEY` | `siliconflow-qwen`, `siliconflow-qwen-coder`, `siliconflow-deepseek` |
| DashScope (Qwen) | `DASHSCOPE_API_KEY` | `dashscope-qwen-plus`, `dashscope-qwen-coder`, `dashscope-qwen-max` |

Any one key gives a complete fleet: workers, rescue models and an
orchestrator. A test enforces this for every provider. More keys give a
deeper ladder, and to add a provider you write a registry file (see below).

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
| `susi-harness budget` | Prints the one-line budget report and any top-up or expiry alert. |
| `susi-harness env` | Lists every variable the harness reads; keys are only marked as set. |
| `susi-harness gateway [--port N] [--host H] [--accounts F]` | Runs the metering gateway. |
| `susi-harness version` | Prints the version. |

`--ledger` and `--catalogue` apply to every command.

- `--ledger` defaults to `SUSI_HARNESS_LEDGER` if set, then
  `out/ledger.jsonl` in a susi workspace, then `~/.susi/harness/ledger.jsonl`.
- `--catalogue` defaults to `deploy/harness/prices.json` when that file
  exists, otherwise the built-in catalogue.

## How models are ranked

The ladder is rebuilt from the live environment on every run:

1. **Eligible**: the model's last real call succeeded, or it has made none;
   the provider has a key and an active budget; and the model can call
   tools. Real calls are the source of truth. A model whose last call failed
   is inactive whatever its budget or balance says, and it stays out until a
   real call succeeds. Each run sends a failed model one small probe call
   once its failure is at least 5 minutes old, and records the result in the
   ledger. If it answers, it is active again. A provider is active with at least $1 left. When the
   remaining balance can't be costed, the provider is active if it answers.
2. **Expiring credit first**: a provider whose credit expires soon leads the
   ladder with all of its models, soonest expiry first. Its strongest rescue
   model orchestrates. Credit lost unused costs nothing to spend.
3. **Cache, then price, then capability, then latency**: cached models come
   first, because a task re-sends the same prompt every turn. Cost is the next
   key, priced at each model's cache rate. When prices are within about 10% of
   each other, the rescue tier prefers the more capable model and every tier
   prefers the faster one, measured from the ledger.
4. **Workers, then rescue**: cheap worker models try first. When they fail,
   stronger rescue models take over. DeepSeek Pro orchestrates when it is
   available. Otherwise the cheapest rescue model does, or failing that the
   cheapest worker.

While a task runs, the ladder keeps this order and only moves models back:

- A model that has failed most of three or more attempts goes behind the
  healthy ones.
- A provider that just failed cools down. One "out of credit" answer is
  enough; any other failure needs two in a row, and a completed call clears
  it. A cooling provider's models go last. Nothing is dropped, so a fleet of
  one model still runs.
- A prompt too long for one model goes straight to the next model with a
  larger context window.
- A rate limit or timeout retries the same model once, which keeps its prompt
  cache warm. Rejected work moves up the ladder. Two models failing the same
  way stop the task, because that points at the task, not the models.

## Settings

Nothing needs setting except one key. Every key and setting is read from
these places, first match wins:

1. the process environment;
2. on Windows, the user environment, then the machine environment (so a value
   set after a shell started is still seen);
3. `./.env`;
4. `~/.susi/.env`.

`.env` files use the usual `NAME=value` form: `#` comments, an optional
`export`, and quoted values are accepted. Run `susi-harness env` to see what
is set.

| Variable | Meaning |
|---|---|
| Provider keys (see Providers) | Any number of them; each enables its provider. A key the provider refuses (401 or 403) is reported as refused, and that provider's models are left off. |
| `SUSI_HOME` | The data home, for the ledger and `.env`. Default `~/.susi`. |
| `SUSI_HARNESS_LEDGER` | The ledger file. |
| `SUSI_HARNESS_BUDGET_<PROVIDER>_USD` | Dollars left at a provider that publishes no balance (Gemini, Anthropic). DeepSeek and Moonshot report their own balance. |
| `SUSI_HARNESS_CREDIT_EXPIRES_<PROVIDER>` | `YYYY-MM-DD` the provider's credit expires. Its models run first until then, and each run alerts in the last 7 days. |
| `SUSI_HARNESS_RPM_<PROVIDER>` | Requests per minute the gateway sends to the provider. Unset means no limit. |
| `SUSI_HARNESS_TPM_<PROVIDER>` | Tokens per minute the gateway lets the provider serve. Unset means no limit. |
| `SUSI_HARNESS_PROVIDERS` | Path to a registry file whose providers and models are added to the built-in ones. A missing or malformed file stops the harness. |
| `ANTHROPIC_WORKSPACE_ID` | Workspace for an Anthropic console user key (process environment only). |

`<PROVIDER>` is the registry id in upper case: `OPENAI`, `ANTHROPIC`,
`GEMINI`, `DEEPSEEK`, `MOONSHOT`, `OPENROUTER`, `SILICONFLOW`, `DASHSCOPE`, and
any provider an overlay adds.

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

A provider with no `key_env` is treated as local: it has no budget to check.
A model without a price still runs. It is ranked last in its tier, and its
tokens are recorded at zero cost. To budget it, add a price to the catalogue.

## The ledger

`out/ledger.jsonl` gets one JSON object per line, written with a single
append so that many agents can share it safely. Each row records:

- the mission, task, attempt, model and real provider;
- tokens, with provider cache hits counted separately;
- the price in force and the settled cost, in micro-dollars;
- start and end times;
- the outcome: `completed`, `model_failed` (with its class), `accepted` or
  `rejected`.

Failures and verdicts cost nothing, but the ranking learns from them.

## Security

- Keys are never printed, logged or written to the ledger. Debug output
  redacts them, and the environment report only says whether a key is set.
- A provider with a published key form (Anthropic: `sk-ant-`) only gets a
  value of that form. A wrong value in one scope is skipped for the next
  scope, with a warning that names the scope and never the value.
- Without `--accounts`, the gateway serves only on loopback. To listen on any
  other address, pass `--accounts`; every caller then needs a bearer token
  with a spend cap and a rate limit.
- On Ctrl-C or SIGTERM, the gateway stops accepting new calls and finishes the
  ones in flight. Every call that reached a provider still gets its ledger row.

## License

Apache-2.0.
