# Harness runbook

The operator's path from an empty shell to a mission SUSI runs on itself.

## Prerequisites

| What | Check |
|---|---|
| `DEEPSEEK_API_KEY` and `GEMINI_API_KEY` in the environment | the gateway reports a missing key on the first call that needs it |
| a `susi-commercial` binary (Susi Commercial Apps' dev workflow) that can run `workflow` and `tasks` | `susi-commercial workflow check` |

Nothing else is installed: the harness routes to the providers directly and the
worker runs in process, so there is no proxy binary and no external agent runtime
to put on `PATH`.

A dev build must not be installed over the release binary (Mandate 48). Use
`target/debug/susi.exe` for a dev mission, or cut a release.

## 1. Start the meter

From `crates/susi-harness`:

```sh
cargo run -p susi-harness --bin harness-gateway -- \
  --port 9188 \
  --catalogue deploy/harness/prices.json \
  --ledger out/ledger.jsonl \
  --upstream direct
```

`--upstream direct` means the gateway resolves each model through the built-in
registry and posts to the provider's own HTTPS endpoint. The gateway refuses a
model the catalogue cannot price. That refusal is the point: spend that cannot be
priced cannot be bounded.

## 2. Author the plan

```sh
susi harness author \
  --gateway http://127.0.0.1:9188 --model deepseek-flash \
  --roadmap .agents/roadmap.json VC-203-001 \
  --workers deepseek-flash,gemini-flash --max-tasks 3 --out plan.json
```

The author is the **supervisor** model. Each task names a worker model. Read the
plan before running it: a task reserves scopes and declares an acceptance, and
the run refuses a plan whose packages, scopes or acceptance do not resolve.

## 3. Watch spend from the command line

```sh
susi harness budget --ledger out/ledger.jsonl
susi harness status --ledger out/ledger.jsonl --events out/events.jsonl
```

`budget` prints the one-line budget report and any top-up alert; `status`
prints the harness API snapshot built from the ledger and event feed only (no
plan, supervisor state, controls or budget). Neither needs a server.

Optionally, for a client that wants a live feed, serve the harness API:

```sh
susi harness api \n  --ledger out/ledger.jsonl --events out/events.jsonl \n  --plan plan.json --state supervisor-state.json \n  --controls out/control.jsonl \n  --catalogue deploy/harness/prices.json --budget-usd 0.50 --port 9181
```

It serves JSON and SSE only, with no page at `/`: `GET /api/status`,
`GET /api/events`, `POST /api/command` and `GET /health` on
`http://127.0.0.1:9181`. Core ships no UI; any visual view of it lives in the
commercial desktop (Susi Commercial Apps).

## 4. Run the mission

```sh
susi harness run plan.json --repo . \
  --gateway http://127.0.0.1:9188 \
  --model deepseek-flash --workers gemini-flash \
  --catalogue deploy/harness/prices.json --budget-usd 0.50 \
  --ledger out/ledger.jsonl --controls out/control.jsonl \
  --parallel 2 --state supervisor-state.json
```

What happens:

1. Each task gets its own worktree, task record and claim.
2. The adapter commits the task record, then runs the native worker in process.
3. If the worker does not pass acceptance, the worktree is reset to that record
   and the next model on the ranked ladder runs.
4. An accepted task lands through `workflow finish`: gate, close, push, PR,
   merge.
5. The supervisor persists after every task. Rerunning the same command skips
   what landed and retries what did not.
6. When the next attempt's worst-case estimate would cross the cap, the task is
   left unstarted and the report says `budget_exhausted`. Nothing is retried.

## 5. Read the result

```sh
susi harness status --ledger out/ledger.jsonl --events out/events.jsonl
```

The report is `done`, `failed`, `skipped`, `unstarted`, `budget_exhausted`,
`paused`, `aborted`. The snapshot carries per-mission spend and per-worker cost;
the harness API snapshot (served with `--controls`) also carries the last
operator commands.

## When something blocks

| Symptom | What to do |
|---|---|
| `budget_exhausted`, tasks unstarted | raise `--budget-usd`, or run a smaller plan. The ledger shows what the attempts cost. |
| a task failed on every model | the error is about the task or the environment, not the model. Fix the acceptance/scope; the ladder deliberately stopped instead of spending more. |
| operator paused | send `{"command":"resume","mission_id":"…"}` to `POST /api/command` (or append a resume command to the control log), rerun the same command. |
| operator aborted | the abort is sticky in that control log. Start a new mission or use a fresh control log. |
| a claim is stale | `susi-commercial tasks audit`; the queue reports the holder and the lease. |
| a model 401s or says the key is unset | the provider key for that model is unset in the environment the gateway runs in. |

## Cautions

- The harness API binds loopback by default. `POST /api/command` passes core's
  `NetGuard`: with `api_auth_token` seeded it needs `Authorization: Bearer
  <token>`; before seeding only loopback callers may write.
- `top_up` records a provider balance event; it does not change the mission's
  hard cap. Raise `--budget-usd` to spend more.
- The ledger is append-only. Never edit it by hand; a correction is a new entry
  in the accounting, not a rewrite of history.

## Budget refusals

A refused attempt is not a failure of the model. The report says `budget_exhausted` and the task stays unstarted. Read the ledger for what the attempts cost, raise the cap with `--budget-usd`, and rerun the same command: the supervisor skips what landed and starts with the task that was refused.
