**START HERE: run `susi workflow check` before you change anything.** It
tells you whether you are in your own worktree, current with `origin/main`,
have the hooks installed and hold a claim — and whether you can actually act on
that: nothing uncommitted and no merge left half-resolved, a claim whose lease
is not about to lapse, a live primary-checkout watcher, and how many of your
finished worktrees are still lying around. It prints the command that fixes each
❌, and warnings (a work in progress, a stale watcher, reclaimable worktrees)
never block work. (No installed susi? `cargo run -q -- workflow check`.)

The agent loop is **atomic per task** (own worktree, always synced with
`origin/main` so parallel agents stay current):

1. **Own worktree, never the primary checkout, never `main`:**
   `scripts/susi-worktree.sh` — no name needed (or `susi workflow start`). It
   creates your branch off the latest `origin/main`, installs the hooks, parks
   the primary checkout and prints `cd <path>`; continue there.
2. **Sync, then claim one task:** if behind, `git fetch && git merge
   origin/main`; then `susi tasks list` and `susi tasks claim <id>`. One live
   claim at a time. Nobody starts a claimed task.
3. **Do the work; every commit ends with** `Task: T-<AGENT>-<n>`. The
   commit-msg hook, the pre-push hook and the CI job "Workflow Compliance"
   reject commits without it.
4. **Sync again, then push:** `git fetch && git merge origin/main`, pass the
   gate (`cargo fmt --all --check`,
   `cargo clippy --workspace --all-targets --locked -- -D warnings`,
   `cargo test --workspace --locked`), then push the branch. Tests are
   hermetic (never touch `~/.susi` or an inherited `SUSI_HOME`).
5. **Close when acceptance passes** (`susi tasks close <id>`), sync again,
   and take the next claim. Do not push to `main`, tag, or cut a release;
   pushed branches open and merge their own PR.

This block is loaded for you: `CLAUDE.md`, `GEMINI.md`,
`.github/copilot-instructions.md`, `.cursor/rules/susi-workflow.mdc` and
`.devin/rules/susi-workflow.md` each point every agent tool here, and
SessionStart hooks (Claude Code: `.claude/settings.json`; Codex:
`.codex/hooks.json`; Devin: `.devin/hooks.v1.json`, all →
`scripts/workflow-session-start.sh`) run the check for you. Keep those pointers identical; this file is the single source.

## Parallel task lifecycle

`workflow start` assigns a unique identity in worktree-local Git config,
including when workers share a Git login. Explicit `SUSI_AGENT` overrides
must also be unique per worker (`CODEX1`, `CODEX2`, etc.).
A worker owns one linked worktree and one live task claim. Several workers
contribute to one larger goal through separate subtasks with explicit
`--dep` dependencies and disjoint `--scope` files/directories; never share a
single task claim or working directory. Declare every implementation path:
`susi tasks claim <id> --scope src/cli --scope tests/workflow_check.rs`.
Use `susi tasks renew <id>` during long work; finish renews before verification
and periodically while waiting for integration. Task, agent and scope reservations are acquired in one atomic remote push.
Overlapping directory/file reservations reject the claim; a concurrent
claim may lose the compare-and-swap and must refresh and retry. Legacy
unscoped claims remain readable; coordinate their scope before overlapping
work. Commit hooks reject staged code outside declared scopes, and the CI job
`Workflow Compliance` re-checks the same rule server-side against every commit
of the pushed branch, together with the branch that holds the claim.

The concrete loop is:

1. `susi workflow start`, then `susi workflow sync` in that worktree.
2. List and claim one available dependency-ready task with its scopes.
3. Implement and commit the work with its open task trailer.
4. `susi workflow finish <id>` synchronizes, runs fmt/clippy/tests and
   acceptance, commits completion, synchronizes and pushes, waits for the
   remote merge, then syncs and releases ownership. It integrates intervening
   merges and reruns the gate before repushing. A failed check/conflict stops
   for repair; rerun finish after committing the repair. Do not claim another
   task until the completion is on remote main. Closing alone retains the
   claim so nobody duplicates work while the PR awaits merge. That wait is
   bounded (`SUSI_FINISH_WAIT_MAX`, default 2 h), and it stops early — with the
   closure and the claim intact, saying what it saw — when the pull request is
   closed or the run for the pushed sha has failed, so a branch that cannot
   merge costs a minute of feedback rather than the whole budget. A lease that
   lapsed during the wait is re-adopted rather than an abort.
   **When the failure is found after acceptance was recorded, the close has not
   been published** (`origin/main` still shows the task open), so revert it with
   a task-only commit — exempt from the trailer rule — and keep fixing under the
   same live claim: citing a closed task is refused, and claiming a new one is
   refused while a completed task is unpublished, so that is the way forward.
5. Repeat sync → claim. No offline freshness or unavailable claim snapshot
   counts as permission to start work.

`workflow start` starts one local primary-checkout watcher per clone.
`susi workflow watch` runs it explicitly; it fetches every 15 seconds and
fast-forwards clean primary `main`, serialized with other local hooks.
Hosted GitHub Actions cannot update a local filesystem: this local watcher
is required while agents run. Dirty primary trees, local-only commits and
in-progress operations are preserved; inspect `.git/susi-primary-watch.log`
and clear the blocker before expecting primary checkout convergence.
Remote integration is serialized and rejects tested heads missing current
main. Other agents see changes once they merge remotely, then synchronize at
their next task boundary; never merge into another agent's dirty worktree.

The full rules follow (identity.json Mandates 48–56 are the constitution).

---

# Agent Engineering Mandates

Hard rules for any code written in this repo — human or AI-generated. These
are enforced mechanically where possible; the rest are reviewed in CI.

## Enforced (compiler — violations fail the build)

- **`unsafe` is forbidden or denied in every crate.**
  - `#![forbid(unsafe_code)]`: susi-abi, susi-adapters-llm, susi-core,
    susi-dsh-cell, susi-error, susi-gawd, susi-gawd-agents, susi-gmcp,
    susi-http-transport, susi-leaf-services, susi-native-client, susi-paths,
    susi-sandbox, susi-sandbox-client, susi-server, susi-universal-cell,
    susi-vendor-agents, susi-vendor-candle, susi-vendor-chrome,
    susi-vendor-cloud, susi-vendor-fastembed, susi-vendor-mcp,
    susi-vendor-mcp-server, susi-vendor-models, susi-vendor-syn,
    susi-vendor-tantivy, susi-vendor-wasmer, susi-vendor-web, xtask, root
    package.
  - `#![deny(unsafe_code)]` + per-function `#[allow(unsafe_code)]` with a
    `// SAFETY:` justification: susi-agents, susi-config, susi-daemon,
    susi-gawd-a2a, susi-gawd-swarm, susi-gemi, susi-gemi-models, susi-tools.
    Justified sites are FFI only (libc syscalls, candle mmap, env mutation in
    tests).
- **No panic-path macros in production code.** Workspace clippy denies:
  `unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`,
  `unreachable`. Every crate has
  `#![cfg_attr(test, allow(...))]` so tests may still unwrap/panic.
- **Exhaustive enum matching**: `clippy::wildcard_enum_match_arm = deny`.
  Catch-alls that are intentional (e.g. `bail!("not a cloud adapter")`) carry
  `#[allow]` + a justification comment.
- `unsafe_op_in_unsafe_fn = deny` workspace-wide.
- `rustfmt` is enforced (`cargo fmt --all --check` in CI and pre-commit).

## The audit-trail pattern

When a lint is denied, exceptions are inline and auditable:

```rust
#[allow(clippy::expect_used)]  // why this invariant can never fail
fn bundled_defaults() -> ...   // include_str! asset — parse failure is a
                               // build bug, not a runtime condition
```

An `#[allow]` without a written justification is a compliance violation.
`tests/architecture_tests.rs` (`every_panic_path_allow_is_justified`) enforces
this for the panic-path lints.

## Writing code here

- Return typed `Result<T, EaiError>` from anything touching I/O, agent state,
  or IPC. Constructors: `EaiError::{sandbox, config, io, network, ...}`.
- Clock timestamps: `duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)`.
- Mutex poisoning: `.lock().unwrap_or_else(|e| e.into_inner())`.
- Prefer infallible constructors over `parse().unwrap()`
  (e.g. `SocketAddr::new(...)` not `"127.0.0.1:80".parse().unwrap()`).
- No new inter-crate edges without necessity; leaf order is
  `paths → error → config → core/sandbox → services → daemon`.
- **susi-* crates declare zero third-party dependencies** — normal, build,
  dev, and target-scoped sections alike. Every crates.io/git crate is owned
  by exactly one `susi-vendor-*` crate: an implementation vendor
  (`susi-vendor-candle`, `susi-vendor-wasmer`, `susi-vendor-mcp*`, …) or a
  pure re-export facade (`susi-vendor-serde`, `susi-vendor-tokio`, …).
  Consumers write `serde = { package = "susi-vendor-serde", path = "../susi-vendor-serde" }`
  so `use serde::…` is unchanged. A new external crate means a new facade —
  `susi_crates_declare_zero_external_dependencies` (architecture_tests)
  enforces it.
- **Vendor *code* lives in vendor crates, not only vendor deps.** Any code
  that speaks a third-party agent, model-provider, or web API must live in a
  `susi-vendor-*` crate: `susi-vendor-agents` (external agent executors +
  framework adapters), `susi-vendor-models` (cloud/frontier/OpenRouter/
  open-weight/HF discovery), `susi-vendor-web` (Open-Meteo, DuckDuckGo),
  `susi-vendor-cloud` (kubectl/docker/aws/gcloud/az CLIs). susi crates
  consume them through re-exports; no third-party endpoint strings or
  vendor protocol logic in non-vendor crates.
- `susi_http_transport::http_call*` returns non-2xx responses; callers must
  check status. `HttpCall::into_bytes(max)` reads `max + 1` and rejects overflow.
  The std-only `susi_paths::loopback` client caps complete responses at 16 MiB.
- Do not redeclare normal dependencies under dev/build scopes unless extra
  test-only or build-only features are required.
- Standalone leaf-service binaries enable only their `service-*` feature;
  root `service-run` may enable `all-services`.
- No stubs: no `todo!()`, `unimplemented!()`, or dead `pub` surfaces left
  "for later".
- **Mandate 48 (Self-Build Order).** The local susi (`~/.susi/bin/susi` + its daemon) is release-only: it builds
  the next susi. Dev builds (`cargo xb`, `target/` binaries) must never
  install there; `scripts/susi-release-sync.sh` is the only path in.
  Dev binaries run as their own instance (`~/.susi-dev`, ports 9190–9194;
  `src/dev_instance.rs`) — verify dev features there, not on the release daemon.

- **Mandate 49 (Worktree Workflow).** Every agent and user works in its own
  git worktree on its own branch (`scripts/susi-worktree.sh <name>`); the
  primary checkout is never committed to, `main` never receives plain commits
  (only merges of `origin/main`-current branches), and `main` is only ever
  fast-forwarded — never force-pushed or deleted. The agent loop is **atomic
  per task**: sync → claim/complete one task → commit → sync → push → sync
  again, so parallel agents always build on latest `origin/main`. Enforced by
  `.githooks/workflow-guard` (called from `pre-commit`, `pre-merge-commit`,
  `pre-push`), enabled by `scripts/setup-dev.sh` (run automatically by
  `cargo xb build`). Overrides (`SUSI_ALLOW_PRIMARY`, `SUSI_ALLOW_MAIN`,
  `SUSI_ALLOW_FORCE`) are explicit env vars, never defaults. Hooks are
  client-side: `--no-verify` bypasses them, so the server-side backstop is
  GitHub branch protection on `main` (no force-push, no deletion, required
  status checks) — a repo setting the owner applies.
  GitHub side is automated by `.github/workflows/auto-merge.yml`: every
  pushed branch gets a PR opened, and it merges itself when the Test gate
  passes, after which the full suite is dispatched on `main`. Branches
  prefixed `wip/` or `nopr/` opt out. Repo settings it needs are applied
  idempotently by `scripts/github-setup.sh`. Finished workflow runs are
  pruned daily by `.github/workflows/cleanup-runs.yml`.

## Workflow mandates (identity.json 49–56 — the constitution, binding on every agent)

The authoritative text is in `.agents/identity.json` (compiled into susi and
read by every agent; delegated agents also receive it in their task via
`susi_core::self_build::BRIEF`). In short:

- **49 Worktree Workflow** — own worktree + branch; never commit on the
  primary checkout or `main`; atomic per-task loop keeps each agent synced
  with `origin/main`.
- **50 Task Queue** — work is recorded and executed from `susi tasks`; one
  live claim at a time; sync-before-claim and sync-before-push; claims are
  atomic git refs with leases; a task closes only when its acceptance check
  passes.
- **51 Non-Conflicting Shared State** — one file per record, namespaced ids,
  union (never delete/renumber) on conflict, merge `origin/main` before push.
- **52 Hermetic Tests** — no test touches `~/.susi`, `~/.susi-dev` or an
  inherited `SUSI_HOME`; `scripts/check-hermetic-tests.sh` is the check.
- **53 Verified Release Gate** — releases only via `susi admin release --cut`;
  the built binary runs `E2E_CHECKS`; fix forward with a new tag.
- **54 Branch-Push Gate** — the branch-push `Test` run gates merges; no
  `pull_request` triggers; a red run is a defect.
- **55 Commands Are Not Missions** — command-shaped input is refused, never
  run as a mission.
- **56 Evidence-Ranked Brain** — providers ranked per task class by recorded
  outcomes; a failed or unfunded model never leads; keys are never removed.

**How git enforces it** (hooks alone are skippable with `--no-verify`, so there
are three layers):

1. `.githooks/commit-msg` refuses a commit with no `Task: T-<AGENT>-<n>` trailer;
   `.githooks/pre-push` (via `workflow-guard`) refuses a feature branch that is
   behind `origin/main` (sync-before-push) and runs
   `scripts/check-workflow-compliance.sh origin/main <pushed sha>` on every
   pushed branch. Exempt: merges, `chore: release vX.Y.Z`, github-actions[bot],
   and commits touching only `.agents/tasks/`. The rule binds commits made after
   the script was introduced; earlier history is not judged. A commit's task
   must have been open in that commit's own tree; citing an already-closed task
   is refused.
2. The CI job **Workflow Compliance** (`test.yml`) runs the same script on every
   branch push, server-side — `--no-verify` cannot skip it. The task must be
   open at that commit under a live `refs/claims/<id>` lease **held on the
   branch being pushed**, and every file each commit touches must be inside that
   claim's `--scope`. So a branch cannot push work for a task another branch
   holds, and cannot reach outside the paths it reserved. Closing the task does
   not skip the claim rule either: `susi tasks close` keeps the lease until the
   closing commit reaches main, so work → close → push passes, while a task file
   hand-moved into `done/` (never claimed) is refused.
3. `scripts/github-enforce.sh` (repo admin) installs a ruleset on `main` with
   **no bypass actor** — every agent pushes with the admin's own key, so an
   admin bypass would be a bypass for all of them. It is phased: `--apply`
   (phase 1) = pull requests only, no force-push, no deletion, which blocks a
   direct push and adds no way to get stuck; `--apply --phase 2` also requires
   the branch-push checks — `Workflow Compliance`, `Format Check`,
   `cargo deny (licenses + advisories)` and `Compile Check (branch pushes)`, all
   of which report on a branch push, which is where a pull request is judged.
   The repository runs **phase 2** (every one of those contexts has been green
   on branch pushes for weeks), so a failing branch cannot be merged by hand
   either — not just by auto-merge.
   `--tags` adds a second ruleset, `susi-release-tags`, over `refs/tags/v*` that
   blocks **moving or deleting** a release tag: re-pointing a published version
   at different code is exactly what pinning a version is for, and Mandate 53's
   "fix forward with a new tag" already forbids the legitimate case. Tag
   *creation* stays open, because the release flow pushes its own tag; the
   provenance of a new tag (on `main`, matching the workspace version) is the
   release job's own check. Also applied.
   The escape hatch is deliberate and audited, not a bypass:
   `--relax` disables the rulesets (GitHub records it), `--apply` restores them,
   `--status` (add `--tags`) shows where they stand.

`tests/architecture_tests.rs::workflow_mandates_are_in_identity_and_their_enforcement_exists`
fails if a mandate goes missing or names an enforcement file that does not exist.

## Test policy

- Property-based testing with `proptest` for state machines and pure
  primitives (see `susi-error::redact::prop_tests`).
- Integration tests live in `tests/`; they carry the panic-capable-macro
  exemption header already.
- **Coverage ratchet (target 100%)**: `.agents/coverage-baseline.json` pins a
  per-crate line-coverage floor; `scripts/coverage-ratchet.sh` (CI `Coverage
  Ratchet` job on `main`) fails when a crate drops below its floor. Raising a
  floor is done by committing the bump with the tests that earn it —
  never lower one. Feature-gated code (e.g. `susi-leaf-services` shells) is
  measured under its own `--all-features` pass; never run workspace-wide
  `--all-features` (cuda/mkl/metal collide).

## Verify before pushing

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

## Multi-agent parallel work

Several agents work concurrently in their own git worktrees on their own
branches, then merge to local `main` and push. These rules keep that
convergent:

- **Evidence IDs are namespaced per agent.** Mint entries as
  `EV-DEVIN-<n>`, `EV-CURSOR-<n>`, `EV-CLAUDE-<n>` (per-agent monotonic).
  Date-stamped `EV-YYYYMMDD-NNN` IDs are the legacy scheme — do not mint
  new ones, since independent counters collide on merge (this forced two
  renumbering passes). A uniqueness check in
  `crates/susi-gawd-agents/build.rs` makes a collision a build failure.
- **On evidence/identity/README merge conflicts, union entries — never
  delete or renumber another agent's entries.** Resolve JSON conflicts by
  keeping both sides' entries, then validate with `jq empty`.
- **One worktree per agent; never share a working directory.** If agents
  must share one, commit path-limited (`git commit -- <file>`) — a plain
  `git commit` sweeps up files another agent staged — and retry on
  `index.lock`. Verified: 8 clones, 6 worktrees of one clone, and 4 agents in
  one directory pushed 5 commits each concurrently with nothing lost.
  `scripts/susi-worktree.sh` now refuses to reuse a directory whose
  `susi.agent` belongs to a different worker — two workers in one tree share one
  token and one branch, so they could renew, close and release each other's
  claim. Finished worktrees are reclaimed with `scripts/prune-worktrees.sh`:
  report-only by default, `--apply` for your own (clean and already merged into
  `origin/main` — anything with uncommitted or unmerged work is kept), `--all`
  for a human cleaning a shared clone. `susi workflow check` reports how many of
  yours are reclaimable.
- **`.agents/evidence.json` appends merge automatically** via the `ledger`
  merge driver (`scripts/setup-dev.sh`, run by `cargo xb build`). Any other
  same-line edit conflicts; resolve by hand, never by force-push.
- **Always `git fetch` + merge `origin/main` before pushing.** Pushes to
  `main` must be fast-forward; compile (`cargo check --workspace`) before
  pushing a merge so fixup commits never ship an uncompiled merge.
- **CI is branch-scoped and affected-crate-scoped.** Feature-branch pushes
  run fmt + cargo deny + `cargo check`, **clippy `-D warnings`** and **the tests
  of** just the crates the diff touches (`scripts/ci-changed-crates.sh`;
  workspace-wide inputs and root-package changes fall back to a full check,
  which keeps `cargo check` alone because a whole-workspace test build on every
  macro-push is what this gate exists to avoid — the *lint* is the exception and
  runs workspace-wide in that case, since a toolchain, lockfile or lint-config
  change is exactly where a lint break hides). Running the affected crates' tests
  here is deliberate: the failures that reach `main` are runtime ones — races,
  flakes, cross-test interference — that a compile check cannot see and the
  shards below would only catch after the merge. Clippy is here because the
  `lint` job is main-only: 1.99.0 deprecated `Atomic::fetch_update` and it
  failed on code nobody had touched. **The compiler is pinned** in
  `rust-toolchain.toml`, and the `Toolchain Pin` job asserts that a runner
  which installs `stable` still resolves it: a toolchain roll once reddened
  `main` with no code change at all (1.99.0 deprecated
  `Atomic::fetch_update`, which failed the clippy job on unchanged code), and
  local gates use the same pinned compiler instead of whatever the host
  happens to have. Bump the pin deliberately, with the fixes the new compiler
  needs. **Build-time downloads are retried**: `ort-sys` fetches ONNX Runtime
  from pyke's CDN once, unretried, for `susi-vendor-fastembed`, so a cold
  build plus one CDN hiccup reddened a shard; the jobs that build it pre-fetch
  with bounded retries (`scripts/ci-prefetch-onnxruntime.sh`, build only —
  never the tests). On `main` and manual dispatch the
  suite runs as six parallel nextest shards (foundation, daemon, gawd,
  gemi, vendor-cells, root-cli) plus a lint job and the live susi-native
  e2e job — the gate is the slowest shard, not one serial workspace build.
  Auto-merge dispatches it on `main` right after each merge, and a 15-minute
  reconciler (`scripts/reconcile-prs.sh`) merges any green PR the events missed,
  comments once on red or conflicting ones and closes PRs idle for 7 days; PRs are gated by
  the branch-push run (there is deliberately no `pull_request` trigger:
  bot-opened PRs' runs are held for approval and die jobs-less on merge).
  A red `main` is **attributed**: the `Main Failure Attribution` workflow
  (`scripts/report-main-failure.sh`) comments once on the pull request whose
  merge produced the failing commit, because the commit that lands is a merge
  commit and parallel agents would otherwise each bisect the same failure to
  find out whose it is.
  The rolling dev release builds only on `workflow_dispatch` or a
  head-commit subject that starts with `[dev-release]`.

## Ratchet items (not yet at zero — do not regress)

- `clippy::pedantic` + `clippy::nursery`: ~2.4k warnings baseline; new code
  should be pedantic-clean even though the lint isn't denied workspace-wide.
- `Mutex`/`RwLock` in hot paths: prefer bounded `flume`/tokio channels for new
  work; existing locks are being migrated incrementally.
- Unwired `susi-daemon` modules: 0 of 56 (`tests/architecture_tests.rs`
  `unreachable_daemon_modules_only_decrease` now asserts none). A new module
  must be reached by real production wiring; discarded probes and type-only
  references do not count.
- Formal verification (`kani`) and `cargo-geiger` unsafe-tree auditing are
  roadmap items — see `.agents/roadmap.json`.

## Autonomous Self-Development (SUSI Building SUSI)

This repository is structurally designed for SUSI to act as the primary intelligence layer for its own development.

- **Primary Mode (Agentic Resolution)**: When operating on GitHub issues or PRs (e.g., via `.github/workflows/susi-builder.yml`), the SUSI binary MUST use native tools to edit code, execute `cargo` / `./build-gpu.sh` commands, and test itself.
- **Delegation Protocol (A2A)**: If a capability gap prevents SUSI from directly building a feature (or an ultimate fallback is triggered), it MUST leverage the A2A protocol to invoke compliant external agents (e.g., `antigravity`, `cursor`, `aider`). 
  - Delegations are written to the `~/.susi/delegations/` ingress directory.
  - External agents invoked this way are bound to the exact same strict mandates listed in this file.
- **Self-Build Order (Mandate 48)**: SUSI, its delegates, and human developers all build dev in `target/`, verify on the dev instance (`~/.susi-dev`, ports 9190–9194), and change the installed susi only by cutting a release (`susi release`), which `scripts/susi-release-sync.sh` promotes. Never install or hot-swap a dev binary into `~/.susi/bin`. SUSI states this contract in every task it gives a coding agent in its own tree (`susi_core::self_build::BRIEF`: delegated external agents and DAG shell nodes), self-patches clear the full fmt/clippy/test gate, and the CI builder (`susi-builder.yml`) is the *released* susi, landing work as a PR (draft if the gate fails).
- **End-to-end verification of the built binary**: the release gate (`susi admin release`) runs the freshly built debug binary on its own newest behaviour (`susi_gawd::admin::E2E_CHECKS`: deterministic, offline, scratch HOME, no daemon) after the smoke missions, and `tests/release_e2e_checks.rs` runs the same list on every test run. A release that ships new behaviour adds a check for it; a check that stops matching fails CI, not the cut.
- **The task queue (`susi tasks`).** Every agent, user and susi itself records work as one file per task under `.agents/tasks/` (`T-<AGENT>-<n>`, namespaced like ledger IDs so adding a task never conflicts) and executes from that queue. `susi tasks add "<title>" --accept "<cmd>"` requires an acceptance command (`cargo …`, `susi …` or `scripts/<file>` only — a task file is not a shell). `susi tasks claim <id>` first fetches origin and refuses from a worktree behind `origin/main` (`git merge origin/main`), then is atomic: it pushes a blob to `refs/claims/<id>` on the shared remote, the server accepts the first push and rejects the rest, and a claim carries a lease (default 4 h) so a crashed agent cannot hold a task — an expired lease is taken over by compare-and-swap, which also clears the previous holder's `refs/claim-agents/<AGENT>` entry (but only while it still points at the claim being taken over). A task with an open dependency cannot be claimed. `susi tasks release <id> [--force]` frees a claim, and it is bound to the branch the claim was taken on: the agent token alone can be shared or collide between workers (`codex-1` and `codex1` both tokenise to `CODEX1`), so freeing a live claim across branches — including moving your own claim to a recreated worktree — is deliberate, with `--force`. `susi tasks close <id>` first fetches `origin` and refuses a task that is already in `done/` on `origin/main` — two agents closing one task on divergent branches happened four times, and no merge driver covers `.agents/tasks/**`, so the conflict is resolved by hand and one record silently wins. It then runs the acceptance command and only a passing run moves the file to `.agents/tasks/done/` (recording who, when, at which commit) and retains the claim until publication; a `cargo test` check that ran zero tests does not count, and an acceptance command is argv rather than a shell line, so quotes in it are refused at `add` time. Tasks may name the roadmap vector they deliver (`susi tasks add --roadmap VC-201-0NN`, validated against `.agents/roadmap.json`); `susi tasks roadmap [--uncovered]` reports each vector's linked open/closed tasks and totals by priority. Coverage is not completion: a vector is done when its `mastery_target` is verified, not merely when its linked tasks close. Commit task files and `done/` moves with the work. Coarse ownership (which agent works which learning-loop stage) is one file per agent in `.agents/lanes/`; it never replaces a claim.
- **Continuous Execution**: The `exec_command` native tool explicitly permits `cargo`, `gh`, `bash`, and `sh` to allow SUSI to test itself and manage source control natively without triggering governance violations.
