# Harness runbook

The operator's path from an empty shell to a mission SUSI runs on itself.

## Prerequisites

| What | Check |
|---|---|
| `DEEPSEEK_API_KEY` and `GEMINI_API_KEY` in the environment | the gateway reports a missing key on the first call that needs it |
| a `susi-commercial` binary (Susi Commercial Apps' dev workflow) that can run `workflow` and `tasks` | `susi-commercial workflow check` |
| the pinned toolchain's `rust-src` component | `rustup component add rust-src` in the checkout; the first `susi local workflow list` then builds the verified standard library the native workflow compiles with |
| `cargo-deny` | the landing gate runs `cargo deny --offline --locked check licenses` |
| a board target and an agent name | `SUSI_BOARD_REMOTE_URL` (or `git config susi.board.remote-url`, or a `board` remote) and `SUSI_AGENT`: a core checkout has no board of its own, and a mission posts its claim before the first task |

The harness itself installs nothing else: it routes to the providers directly
and the native worker runs in process, so there is no proxy binary and no
external agent runtime to put on `PATH`. The last three rows are what the
native workflow under it needs; on Linux that workflow also needs a C compiler
and the system ICU and OpenSSL 3 headers
([NATIVE-BUILDS.md](../toolchain/windows-stdlib/NATIVE-BUILDS.md)). With no
provider key at all, see "No provider key" below.

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

### No provider key: a subscription CLI worker

```sh
SUSI_HARNESS_CLI_GOVERNED=1 susi harness run plan.json --repo . \
  --gateway http://127.0.0.1:9188 --model cli:claude:sonnet --timeout 2700
```

`--model cli:<tool>[:<model>]` runs an installed coding CLI as the worker on
its own subscription: it needs no provider key and makes no gateway call
(`--gateway` is still a required flag, and nothing needs to listen there).
`SUSI_HARNESS_CLI_GOVERNED` runs that worker governed
([harness.md](harness.md), "A governed CLI worker"). A subscription mission
gives each task one attempt, with no ladder to escalate on. A plan entry with
no `task` mints its own task record, named `T-LOCAL…` after the worktree.

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
| the landing failed: `native integration failed … candidate retained` | the accepted work and its claim are kept, and the message names the gate's log. Fix what the gate found (it may be the base, not the candidate: a test already red on main stops every landing), then rerun the mission, which re-verifies the retained candidate and lands it; `susi local workflow integrate <claim>` does the same by hand. |
| the landing is refused at once: `integration history does not contain the claim base` | the local integration head holds a landing that is not on the fetched main yet, so a claim based on that main cannot build on it. Publish the local integration (`susi-commercial workflow submit-main refs/susi/integration/current`), fetch, and rerun the mission. A main that only moved through other agents' pushes needs nothing: `integrate` builds on the fetched main by itself. |
| the landing gate fails in `host_only_privacy`: `this test needs a routed private or link-local IPv4 address` | the host's only routed address is outside the reach `local_only` keeps local (a cloud container, or a host with only a public address), so the privacy tests cannot run and no landing passes its gate there. Land from a host with a LAN address, or run the landing inside a private network namespace that has one (`unshare -n`, loopback up with a private address and a default route through it): the gate is offline and needs no other network. |
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
