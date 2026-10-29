# Cloud sessions: the clone is the worktree, and nothing needs the laptop

A **cloud session** is an agent run that clones this repository into a
disposable container — Claude Code on the web (`claude.ai/code`) today, any
harness that does the same tomorrow. It has no laptop behind it: no shared
primary checkout to keep parked, no sibling worktrees sharing a cargo target,
no watcher to keep a primary converging, and no way (nor any need) to reach
the operator's machine. Everything the Susi loop needs — build, test, claim,
commit, push, publish — runs inside the container, through the container's
git and GitHub access.

Two rules follow, and the tooling enforces both:

1. **The clone the harness made, on the branch it checked out, is the
   worker's own worktree** in the sense of Mandate 49. Nobody runs
   `scripts/susi-worktree.sh` to make a sibling; `susi workflow check` passes
   "own worktree" there; the hooks allow commits there; `main` and a detached
   HEAD are still refused.
2. **A cloud session never asks for the operator's laptop.** Not Remote
   Control, not a "computer folder", not a button to press on a phone, not a
   permission that only a human at a keyboard can grant. If something is
   missing from the container (a host the network policy refuses, a tool
   that is not installed), the session says so and carries on with what does
   not depend on it; it does not stand in for the missing part with a
   laptop.

## How a clone knows it is a cloud session's

The fact is recorded **on the clone**, in its git config, never read from the
process environment by the code that acts on it:

```
susi.cloud = true          # this clone is a cloud session's own worktree
susi.cloudTool = CLAUDE    # the agent tool the harness runs: the identity prefix
```

`scripts/cloud-session.sh` is the single source of the rule, and the susi
binary (`susi_gawd::cloud_session`) applies the same one:

| Command | Meaning |
|---|---|
| `scripts/cloud-session.sh` | exit 0 when this clone is a cloud session's |
| `scripts/cloud-session.sh --detect` | exit 0 when the *environment* is a cloud harness (prints the tool); marks nothing |
| `scripts/cloud-session.sh --mark` | mark this clone when `--detect` holds (`--force` to mark regardless) |
| `scripts/cloud-session.sh --tool` | the tool the clone was marked with |
| `git config --unset susi.cloud` | take the mark back |

The session-start hook (`scripts/workflow-session-start.sh`, run by
`.claude/settings.json` and `.codex/hooks.json`) marks the clone it was
started for — and only that one: it requires `CLAUDE_PROJECT_DIR` to be the
repository it runs in, so a fixture repository a test runs the script in is
never marked. Only `--detect` and `--mark` look at the harness's own variable
(`CLAUDE_CODE_REMOTE_ENVIRONMENT_TYPE`, which Claude Code sets in its cloud
environments and nowhere else).

Why the mark and not the environment: a test binary run *inside* a cloud
container inherits that environment. If the hooks and the check read it,
every fixture repository in the test suite would turn into a cloud clone
there, and `the_primary_checkout_is_refused_with_the_fix` would fail in the
cloud while passing on a laptop. The mark keeps a fixture a laptop's clone
until a test marks it.

`SUSI_CLOUD_SESSION=1` / `SUSI_CLOUD_SESSION=0` is the operator's (or a
test's) explicit word, and beats the mark either way.

## What changes once the clone is marked

| Where | Laptop clone | Cloud session's clone |
|---|---|---|
| `susi workflow check` → own worktree | ❌ in the primary checkout | ✅ on the harness's branch (❌ still on `main` / detached) |
| `susi workflow check` → own identity | the primary carries none | assigned `<TOOL><BRANCH>`, e.g. `CLAUDECCRC9F023D1SDLJMO`, as claim token and git author, with the harness's signing email (its global `user.email`) so the signed commits show as verified on GitHub |
| `susi workflow check` → primary watcher | ⚠️ when no heartbeat | ✅ not needed: nothing to keep converging |
| `.githooks/workflow-guard commit/merge` | refuses the primary checkout | allows it (still refuses `main`) |
| `scripts/check-worker-identity.sh` | skips the primary checkout | judges it like any worktree (no `PRIMARY`, no login as author) |
| `scripts/susi-worktree.sh`, `susi workflow start` | create a sibling worktree, park the primary, start the watcher | install the hooks and print `cd <this clone>`; refuse on `main` |
| `scripts/park-primary.sh` | parks the primary at `origin/main` | does nothing (silently for `--sync-only`) |
| `scripts/ensure-watcher.sh` | restarts a stale watcher | nothing to heal |
| `scripts/workflow-session-start.sh` | links the shared target, syncs the primary | installs the ONNX Runtime from GitHub, starts the build in the background, prints the cloud guidance |

Everything else — the task queue, claims, the `Task:` trailer, the gate, the
close receipt, the pull request that opens and merges itself — is the same
loop as anywhere else. The identity is `<TOOL><BRANCH-ID>`: the harness's
branch (`ccr-<id>`) names no tool itself, so the tool recorded with the mark
prefixes it, and the branch keeps it unique per session.

## Building in a cloud container

A cloud container starts cold: no installed `susi`, no `target/`. Two things
are specific to it:

* **The ONNX Runtime.** `susi-vendor-fastembed` builds `ort-sys`, whose build
  script downloads a prebuilt ONNX Runtime from pyke's CDN (`cdn.pyke.io`).
  The container's egress policy allows GitHub and the package registries and
  refuses that CDN (`CONNECT proxy failed: proxy server responded 403`), so a
  cold build died in `ort-sys` before any workspace crate compiled.
  `scripts/cloud-onnxruntime.sh` fetches Microsoft's release of the same
  version from GitHub (checksum pinned), and points cargo at it through
  `$CARGO_HOME/config.toml` — `ORT_LIB_LOCATION`, `ORT_PREFER_DYNAMIC_LINK=1`
  and an rpath, so every binary runs without an `LD_LIBRARY_PATH` — never
  through the repository's `.cargo/config.toml`, which every laptop shares.
  The session-start hook runs it; it is idempotent.
* **The background build.** The check needs the binary, so the hook starts
  `scripts/target-guard.sh --run cargo build -q` in the background (log:
  `.git/susi-cloud-build.log`). The guard serializes a second build behind
  it, so `scripts/target-guard.sh --run cargo build -q` by hand simply waits
  for it. `SUSI_CLOUD_PREBUILD=0` opts out; `SUSI_CLOUD_ONNXRUNTIME=0` skips
  the runtime step.

Then the loop is the ordinary one, with the dev binary:

```sh
target/debug/susi workflow check
target/debug/susi tasks list
target/debug/susi tasks claim <id> --scope <path> [--scope <path>…]
# …work; every commit ends with `Task: <id>`…
target/debug/susi workflow finish <id>     # gate, close, push, receipt, release
```

Run cargo through the guard as everywhere else (`scripts/target-guard.sh
--run cargo test -p <crate>`); `finish` and `tasks close` guard themselves.

## GitHub from the container

Measured, not assumed (a `git push --dry-run` shows none of it):

* The container's git proxy pushes **branches** (`refs/heads/*`), any branch
  name, and nothing else. A push to `refs/claims/*`, `refs/closed/*` or any
  other queue namespace is answered with HTTP 403 at receive-pack, and so is
  a branch deletion ("the proxy rejects branch deletions and pushes of
  anything other than a branch", code.claude.com/docs/en/cloud-environments).
* `gh api` reads the repository's REST API, and every write path is refused
  ("Write access to this GitHub API path is not permitted through this
  proxy"): no refs, blobs, trees or contents. `gh auth status` reports the
  token as invalid because the endpoint it probes is not proxied — ignore it;
  `gh api repos/<owner>/<repo>` answers.
* The harness's clone is shallow; the hooks and the compliance script walk
  the whole history (`fatal: bad revision '^…^'`), so the session-start hook
  runs `git fetch --unshallow` in a cloud clone.
* An Actions job's `GITHUB_TOKEN` pushes any ref the rulesets leave open —
  `scripts/auto-merge-pr.sh` already writes `refs/merged/*` and
  `refs/verified/*` that way — and GitHub runs the workflow files *of the
  pushed commit* for a push. (A `workflow_dispatch` is accepted too, but only
  through the GitHub MCP server, which the `susi` binary cannot call, and only
  for a workflow file already on `main`.)

So the queue's refs travel as **request branches**. When `susi tasks claim`,
`renew`, `release` or `close` has its compare-and-swap push refused while
every lease it carried still holds (a lost race is a lost race, and is never
relayed), and every ref it touches is one of the queue's (`refs/claims/`,
`refs/claim-agents/`, `refs/claim-generation/`, `refs/closed/`,
`refs/abandoned/`), it writes the transaction — every lease, every ref
update, the blobs' bytes, an expiry — into the message of a commit on
`refs/heads/susi-queue/<agent>/<expires>.<nonce>` whose tree is HEAD's,
pushes that branch, and waits for it to disappear.
`.github/workflows/queue.yml` runs for the push: it replays the push with the
job's token through `scripts/queue-relay.py` (which refuses every other
namespace, and any request past its expiry), deletes the request, and sweeps
any request whose expiry has passed — the cloud cannot delete a branch, so
the relay is the one place that can. The client then judges the refs only its
transaction writes (the push was atomic; the shared generation ref, which
every claim rewrites, never decides), so what it reports is what the remote
holds. The hooks and CI treat `susi-queue/**` as queue traffic, not code: no
task trailer, no compliance run, no pull request.

Nothing to bootstrap: the workflow travels in the request's own tree, so a
branch's first request runs it before it is on `main`. The direct push is
always tried first, so a laptop clone never relays; `SUSI_QUEUE_TRANSPORT=
direct` forbids the relay and `relay` skips the direct attempt.
`SUSI_QUEUE_WAIT_SECS` (300) is a request's lifetime: past it the relay
refuses the request, so a client that stopped waiting and said "it will
never be applied" is never contradicted by a late run. The workflow has no
concurrency group on purpose: GitHub keeps one pending run per group and
cancels the rest, which would strand requests; the leases already serialize
replays that touch the same refs. The whole loop — claim, work, commit,
close, push, release — therefore runs from the container, and the `Workflow
Compliance` check sees the claim it needs.

The pushed branch opens and merges its own pull request
(`.github/workflows/auto-merge.yml`); a cloud session does not open one by
hand, and never a draft, which the merge machinery cannot merge.

## Permissions

The loop's commands need no human in the loop: `cargo`, `git`, `gh api`, the
repository's `scripts/`, the hooks and the dev binary. `.claude/settings.json`
is where a repository pre-approves them for Claude Code sessions and denies
the tools that would reach a laptop (`request_computer_folder`,
`list_computer_folders`, the remote-devices / computer-use / browser tool
families). That file is reserved by a live claim at the time of writing; the
queue holds the task that adds the rules (see "Follow-ups").

## What a cloud session never does

* run `scripts/susi-worktree.sh` to make a sibling worktree, start a watcher,
  or park a primary checkout — there is none;
* ask for Remote Control, a computer folder, or any confirmation that needs
  the operator's device;
* install or promote a release (`~/.susi/bin`, `scripts/susi-release-sync.sh`,
  Mandate 48) — the container has no release instance;
* open a pull request by hand or as a draft — the push does it.

## Follow-ups

Queued as tasks (`susi tasks list`), because the files were reserved by
another live claim when this landed, or because the work cannot be verified
from inside a cloud session until it is on `main`:

* `scripts/parallel-workflow.sh` — `sync` (and so `finish`) still refuses
  "Use your own linked worktree." in the primary checkout; until it reads the
  mark, a cloud session runs the finish steps by hand (gate → `tasks close`
  → commit → merge `origin/main` → push → `tasks release --force`), which
  AGENTS.md names as the supported path outside the harness.
* `.claude/settings.json` — the permission allow/deny lists above.
* `AGENTS.md` — the "Cloud sessions" section of the START HERE block.

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| `❌ own worktree — this is the primary checkout` in a cloud session | the clone is not marked (the hook did not run, or ran for another directory) | `scripts/cloud-session.sh --mark` (needs the harness's variable; `--force` otherwise) |
| `ort-sys failed to download prebuilt binaries … 403` | the CDN is refused by the network policy | `scripts/cloud-onnxruntime.sh`, then build again |
| `queue: … refused the direct push … relaying it through .github/workflows/queue.yml` | normal in a cloud session: the proxy carries branches only | nothing; the command waits for the run (a minute or so) and reads the refs back |
| `queue relay: susi-queue/… was not consumed in 300s and has expired` | no run came: the branch's tree has no `.github/workflows/queue.yml` (a branch older than the relay), or Actions is off | merge `origin/main` and retry; the expired request is never applied, and the next relay run sweeps it (do not try to delete it: the proxy refuses branch deletions) |
| `… (relayed as susi-queue/…; origin does not carry the requested refs/claims/…)` | the compare-and-swap lost on the server: another agent moved the ref between the read and the replay | the same as any lost race: `susi tasks list`, then retry |
| `gh auth status`: "The token in GH_TOKEN is invalid" | an unproxied probe | ignore; `gh api …` reads work |
| `susi tasks claim`: "was claimed concurrently with another agent" on a quiet queue, with no `queue:` line before it | a `susi` built before the relay: it read every refused push as a lost race | build this branch's `susi` (`scripts/target-guard.sh --run cargo build -q`) and retry; the relay reports what it does |
| pre-push: `fatal: bad revision '^…^'`, "cannot compute the commit range" | the harness's clone is shallow | `git fetch --unshallow origin` (the session-start hook does it) |
| `Use your own linked worktree.` from `susi workflow sync` / `finish` | `scripts/parallel-workflow.sh` does not read the mark yet | the by-hand finish above |
| a fixture test behaves like a cloud clone | something exported `SUSI_CLOUD_SESSION=1`, or the fixture was marked | unset it; fixtures are marked only by a test that means to |
