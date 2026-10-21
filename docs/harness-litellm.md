# Harness + LiteLLM

LiteLLM sits between the harness gateway and the model providers. The gateway
stays the control point — it attributes every call to a mission and task,
prices it and writes the ledger — while LiteLLM owns the provider layer: one
OpenAI-compatible surface over DeepSeek, SiliconFlow, DashScope and Gemini,
with a response cache.

```
agent (OpenClaw) → harness gateway (meter, attribute, price, ledger)
                → LiteLLM (cache, route, retry)
                → deepseek / siliconflow / dashscope / gemini
```

The cache is exact-match on the request body, so a repeated call is served by
LiteLLM and costs nothing. The gateway recognises LiteLLM's
`x-litellm-cache-key` response header and records the row with `cached: true`
and `cost: 0`, so a cache hit never inflates the ledger; the token counts are
still recorded, because they describe the answer that was served. Streamed
responses are forwarded as they arrive and are not cache-priced.

## Run it

```sh
harness-litellm --config deploy/litellm/config.yaml --port 4000
```

The launcher starts `litellm` with that config, waits for
`/health/liveliness`, and warns about any key the config references but the
environment does not set. `litellm` must be on `PATH` (or pass `--litellm`).

Then point the gateway at it:

```sh
harness-gateway \
  --port 9188 \
  --catalogue deploy/litellm/prices.json \
  --ledger out/ledger.jsonl \
  --upstream http://127.0.0.1:4000/v1/chat/completions \
  --provider openai \
  --api-key-env LITELLM_MASTER_KEY
```

A model is named by its LiteLLM `model_name` — `deepseek-chat`,
`siliconflow-qwen`, `dashscope-qwen-max`, `gemini-flash` — and priced by the
same name in `deploy/litellm/prices.json`. An unpriced model is refused at the
gateway, because unpriced spend cannot be accounted.

OpenClaw reaches the gateway with `--model harness/<model_name>`.

## Keys

| Key | Models |
|---|---|
| `DEEPSEEK_API_KEY` | `deepseek-chat`, `deepseek-reasoner` |
| `SILICONFLOW_API_KEY` | `siliconflow-qwen`, `siliconflow-deepseek` |
| `DASHSCOPE_API_KEY` | `dashscope-qwen-plus`, `dashscope-qwen-max` |
| `GEMINI_API_KEY` | `gemini-flash` |
| `LITELLM_MASTER_KEY` | optional; requires a bearer key on the proxy |

A model whose key is unset fails only when it is called.

## Caching

`litellm_settings.cache: true` with `type: local` is an in-process cache with a
24-hour TTL. For a cache shared by several supervisors, set `REDIS_URL` and
change `type` to `redis`:

```yaml
cache_params:
  type: redis
  host: os.environ/REDIS_HOST
  port: os.environ/REDIS_PORT
```

Provider-side prompt caching (DeepSeek's cache-hit tokens, for example) is
separate and already priced: `usage` keeps the cache-hit count and the ledger
records it at the hit rate.

## Adding a model

1. Add a `model_list` entry in `deploy/litellm/config.yaml`.
2. Add the same `model_name` to `deploy/litellm/prices.json` with its per-million
   prices in micro-USD (`input_hit`, `input_miss`, `output`).
3. Restart `harness-litellm`; the gateway picks the model up by name.

The prices in `prices.json` are a starting point — check them against the
provider's current price list before trusting a report.

## Inside susi

The mission loop is reachable from the susi binary, not only from the
`harness-*` bins:

```sh
susi harness litellm --config deploy/litellm/config.yaml --port 4000

susi harness author --gateway http://127.0.0.1:9188 --model deepseek-chat \
  --roadmap .agents/roadmap.json VC-203-001 --out plan.json

susi harness run plan.json --repo . --gateway http://127.0.0.1:9188 \
  --model deepseek-chat --parallel 2

susi harness status --ledger out/ledger.jsonl
```

`author` writes a validated plan, `run` drives the supervisor (prepare in
parallel, land in order, resume from state), `status` prints the cockpit
snapshot, and `litellm` starts the proxy the gateway points at — nothing about
the orchestration lives outside the repository.
