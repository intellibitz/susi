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

## Quick start

```bash
export DEEPSEEK_API_KEY=...        # any one provider's key is enough
susi-harness fleet                 # the orchestrator and ladder your keys give
susi-harness budget                # what each provider has left, in one line
susi-harness gateway               # an OpenAI-compatible endpoint on 127.0.0.1:9180
```

Point any OpenAI-compatible client at `http://127.0.0.1:9180/v1` and choose a
model by its id: `deepseek-flash`, `gemini-flash`, `kimi-flash`,
`deepseek-pro`, `gemini-pro`, `kimi-k3`, `claude-haiku`, `claude-sonnet` or
`claude-opus`. The gateway sends each call to that model's provider with that
provider's key, prices the call, and appends a row to the ledger
(`out/ledger.jsonl`) before it returns the answer.

## Commands

| Command | What it does |
|---|---|
| `susi-harness fleet [--workers auto\|m1,m2]` | Prints the orchestrator and the escalation ladder. |
| `susi-harness models` | Lists the models your keys make usable, and the ranking evidence. |
| `susi-harness models --verify [--provider ID]` | Checks each provider live: key accepted, models present. |
| `susi-harness budget` | Prints the one-line budget report and any top-up or expiry alert. |
| `susi-harness env` | Lists every variable the harness reads; keys are only marked as set. |
| `susi-harness gateway [--port N] [--host H] [--accounts F]` | Runs the metering gateway. |
| `susi-harness version` | Prints the version. |

`--ledger` (default `out/ledger.jsonl`) and `--catalogue` (default
`deploy/harness/prices.json`, falling back to the built-in catalogue) apply to
every command.

## How models are ranked

The ladder is rebuilt from the live environment on every run:

1. **Eligible**: the provider has a key, its budget is active, and the model
   can call tools. A provider is active with at least $1 left. When the
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

Every setting is read from the process environment first, then the user
environment, then the machine environment. A value set after a shell started
is still seen. Run `susi-harness env` to see what is set.

| Variable | Meaning |
|---|---|
| `DEEPSEEK_API_KEY`, `GEMINI_API_KEY` / `GOOGLE_API_KEY`, `MOONSHOT_API_KEY` / `KIMI_API_KEY`, `ANTHROPIC_API_KEY`, … | Provider keys. Any number of them; each one enables its provider. |
| `SUSI_HARNESS_BUDGET_<PROVIDER>_USD` | Dollars left at a provider that publishes no balance (Gemini, Anthropic). DeepSeek and Moonshot report their own balance. |
| `SUSI_HARNESS_CREDIT_EXPIRES_<PROVIDER>` | `YYYY-MM-DD` the provider's credit expires. Its models run first until then, and each run alerts in the last 7 days. |
| `SUSI_HARNESS_RPM_<PROVIDER>` | Requests per minute the gateway sends to the provider. Unset means no limit. |
| `SUSI_HARNESS_TPM_<PROVIDER>` | Tokens per minute the gateway lets the provider serve. Unset means no limit. |
| `SUSI_HARNESS_PROVIDERS` | Path to a registry file whose providers and models are added to the built-in ones. A missing or malformed file stops the harness. |
| `ANTHROPIC_WORKSPACE_ID` | Workspace for an Anthropic console user key (process environment only). |

`<PROVIDER>` is the registry id in upper case: `DEEPSEEK`, `GEMINI`,
`MOONSHOT`, `ANTHROPIC`, and any provider an overlay adds.

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
