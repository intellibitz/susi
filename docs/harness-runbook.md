# Harness runbook

Run an authorized mission from an isolated checkout, distinguish local
integration from remote publication, and retain its working state outside
tracked source. Follow the [active contract](../AGENTS.md) and
[cross-host coordination guide](harness-coordination.md).

## Prerequisites

- Use a development candidate or qualified installed binary with the native
  workflow entry points. Keep debug builds separate from the installed release.
  Prepare the pinned toolchain, matching source component and platform build
  prerequisites through the [native bootstrap](native-bootstrap.md); license and
  standard-library qualification remain required.
- Select priced model IDs from `susi harness models` and the chosen price
  catalogue. Supply only the credentials those endpoints require to the gateway.
  A model that cannot be priced is refused before spending.
- Install and admit the providers required for workflow reconciliation. The five
  filesystem tools, scorecards and storage audits prefer registered providers,
  retaining standalone behavior after healthy discovery proves provider absence.
  The worker's `run_command` keeps its local policy. See the
  [implementation-service contract](architecture.md#implementation-services-and-protocol-adapters).
- Use an owned agent identity and authorized private board target, configured
  through `SUSI_BOARD_REMOTE_URL`, `susi.board.remote-url` or a `board` remote.
  The current mission path posts coordination messages before and after running;
  those messages must be authorized. It refuses the protected public source
  repository as a board destination.

For the shell examples below, run from the repository root. Set `SUSI_BIN`,
`HARNESS_BIN` and `GATEWAY_BIN` to the actual candidate executables (`susi`,
`susi-harness` and `harness-gateway`). Set `RUN_DIR` to an existing private
absolute directory outside the checkout. Set `PLANNER_MODEL`, `WORKER_MODEL` and
`WORKER_MODELS` to admitted, priced model IDs; the last may be comma-separated. Equivalent
native shell argument lists work on supported platforms. No command here
installs a provider or grants permission implicitly.

## 1. Start the meter

```sh
"$GATEWAY_BIN" --port 9188 \
  --catalogue deploy/harness/prices.json \
  --ledger "$RUN_DIR/ledger.jsonl" --upstream direct
```

`--upstream direct` resolves models through the gateway's registry and sends
requests to their configured endpoints. Unpriced models are refused. Keep this
process's credentials and runtime home private; select a different owned port
if the example port is occupied.

## 2. Author the plan

```sh
"$SUSI_BIN" harness author \
  --gateway http://127.0.0.1:9188 --model "$PLANNER_MODEL" \
  --roadmap .agents/roadmap.json VC-203-001 \
  --workers "$WORKER_MODELS" --max-tasks 3 --out "$RUN_DIR/plan.json"
```

Choose a current roadmap vector for the actual task. Inspect the generated plan:
each task needs a resolvable package, confined scope and meaningful acceptance.
The author model proposes work; it does not authorize changes or certify their
completion. To select existing dependency-ready queue tasks instead, use
`harness plan` as described in [the harness guide](harness.md#draining-the-roadmap-queue).

## 3. Watch spend and control

```sh
"$SUSI_BIN" harness budget --ledger "$RUN_DIR/ledger.jsonl"
"$SUSI_BIN" harness status \
  --ledger "$RUN_DIR/ledger.jsonl" --events "$RUN_DIR/events.jsonl"
```

`budget` reports spend and top-up alerts. `status` builds a snapshot from the
ledger and event feed; it does not load the plan, supervisor state or controls.
For a client that needs those inputs and a live feed:

```sh
"$SUSI_BIN" harness api \
  --ledger "$RUN_DIR/ledger.jsonl" --events "$RUN_DIR/events.jsonl" \
  --plan "$RUN_DIR/plan.json" --state "$RUN_DIR/supervisor-state.json" \
  --controls "$RUN_DIR/control.jsonl" \
  --catalogue deploy/harness/prices.json --budget-usd 0.50 --port 9181
```

This serves JSON and SSE at `/api/status`, `/api/events`, `/api/command` and
`/health`, with no page at `/`. It binds loopback by default. With a seeded
`api_auth_token`, writes to `/api/command` require its bearer token; before
seeding, only loopback callers may write. Preserve the existing `NetGuard`
authorization rather than exposing an unauthenticated control endpoint.

## 4. Run the mission

```sh
"$SUSI_BIN" harness run "$RUN_DIR/plan.json" --repo . \
  --gateway http://127.0.0.1:9188 \
  --model "$WORKER_MODEL" --workers "$WORKER_MODELS" \
  --catalogue deploy/harness/prices.json --budget-usd 0.50 \
  --ledger "$RUN_DIR/ledger.jsonl" --controls "$RUN_DIR/control.jsonl" \
  --rsi-log "$RUN_DIR/rsi-loop.jsonl" --adapter-state "$RUN_DIR/adapter" \
  --parallel 2 --state "$RUN_DIR/supervisor-state.json"
```

The adapter reserves each task's worktree, claim and scope, then runs the worker
and verifies acceptance. Failed attempts follow the configured attempt and
escalation policy; retained work and claimed candidates are not disposable.
Accepted work is committed and passed to native `local workflow integrate`.
This path has no hosted review or pull-request step.

Preparation may run in parallel; landing is serial. The supervisor persists
after each task and skips tasks already recorded as done on resume. Its `done`
means native local integration succeeded, not that the remote accepted a push
or that a production release is qualified. When the next attempt's worst-case
estimate would exceed the cap, it remains unstarted with `budget_exhausted`.

### A configured CLI worker

An installed CLI may provide the worker through `cli:<tool>[:<model>]` and its
own authentication. Set `CLI_WORKER` to an actual admitted entry. Use a plan
whose task models select that same CLI route or omit their model override; a
plan pinned to a conflicting billing route is refused:

```sh
SUSI_HARNESS_CLI_GOVERNED=1 "$SUSI_BIN" harness run "$RUN_DIR/plan.json" \
  --repo . --gateway http://127.0.0.1:9188 --model "$CLI_WORKER" \
  --timeout 2700 --state "$RUN_DIR/supervisor-state.json" \
  --controls "$RUN_DIR/control.jsonl" --ledger "$RUN_DIR/ledger.jsonl" \
  --rsi-log "$RUN_DIR/rsi-loop.jsonl" --adapter-state "$RUN_DIR/adapter"
```

This worker does not call the model gateway, although `--gateway` remains a
required argument. Its own authentication and usage terms still apply. The
governed mode requires a supported permission profile and refuses an unknown
profile before launch; see [governed CLI workers](harness.md#a-governed-cli-worker).
A CLI-only mission has one attempt and no model escalation ladder. A plan entry
without a task ID creates a native task record for its worktree.

## 5. Inspect and publish

Inspect the supervisor report and the retained local integration receipt.
Report states include done, failed, skipped, unstarted, budget exhaustion,
pause and abort. Read spend and errors with the status command above. A missing
state file, empty history or coordination message is not completion evidence.

The native integration reference is `refs/susi/integration/current`. When it
holds the intended accepted candidate, publish it through the admitted workflow
provider using the standalone harness command:

```sh
"$HARNESS_BIN" submit-main --repo . \
  --rev refs/susi/integration/current --remote origin
```

This reconciles onto observed remote main and performs a normal fast-forward
publication. Inspect the returned receipt and confirm the remote contains the
accepted head. If publication succeeded but archival cleanup has a warning,
retain that accepted result and repair cleanup; do not resubmit the original
mutation. An uncertain transport result requires remote/receipt inspection
before any retry.

After confirmed acceptance, the receiver retains verified native task records in the
clone's Git metadata (`susi-records/completed`) and removes active source copies
through a confirmed cleanup commit. It does not create growing
`.agents/tasks/done/` histories. Conflicting local records block cleanup with a
warning; missing history never becomes proof. Local integration alone leaves
pending intent available until publication settles.

## When something blocks

| Symptom | Response |
|---|---|
| Provider unavailable or permission refused | Inspect the admitted MCP entry and exact executable permissions. Install/repair through the authorized implementation workflow; do not fake a successful empty result or bypass admission. |
| `budget_exhausted` | Inspect spend, use a smaller plan or an explicitly authorized higher cap, then resume the same state. `top_up` records a balance event; it does not raise the mission cap. |
| Acceptance fails on every attempt | Inspect the task, environment and retained output. Repair the implementation or a demonstrably incorrect acceptance command; do not weaken a valid requirement to obtain a pass. |
| Paused or aborted | Resume a pause through the authenticated control API or the owned control log. Abort remains sticky in that log; a new mission/control log requires an intentional new run. |
| `native integration failed … candidate retained` | Keep the candidate and claim, inspect the gate log and fix its cause. Resume re-verifies retained work; `susi local workflow integrate <claim>` is the same native landing path. |
| `integration history does not contain the claim base` | Inspect the local integration ref and fetched main, publish a valid pending integration if needed, fetch and reconcile. Do not reset another claim or force the ref. |
| A required platform or privacy fixture cannot run | Supply its actual isolated platform/network prerequisites or qualify on an appropriate host. Do not mark a skipped or unavailable check as passing. |
| A claim appears stale | Inspect its owner and lease on the actual coordinator. Release only work you own or have explicit authority to recover. |
| A model reports missing credentials or rejects them | Repair only that endpoint's authorized credential configuration. Do not print secrets or remove or disable a credential merely because a call failed. |

Keep live accounting append-only and preserve the state needed for recovery.
Disposable operational history may be discarded after its consumers no longer
need it; do not publish it as source. Release qualification, required legal
notices and artifact-bound provenance are separate obligations. The whole agent
engine and persistent identifier migrations remain incomplete; this runbook
does not claim the core-size goal or protocol certification is complete.
