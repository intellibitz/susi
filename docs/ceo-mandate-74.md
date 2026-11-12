# CEO Mandate 74: Autonomous CTO Ecosystem Mandate — <10s Core, 5 Pillars, Remote Pull & Merge, Distilled Releases, and Open-World Harvest

Effective 2026-11-11 (extended 2026-11-12 by [CEO Mandate 75](ceo-mandate-75.md),
which codifies the **Raw Source (`main`) -> Distill (`release`)** pipeline,
autonomous cloud raw-source feature pickup with Core-vs-Commercial routing, and
**Pure `Apache-2.0` ONLY** harvest compliance, winning where older text differs).
Binding on every agent that touches `intellibitz/susi` and
the Susi AI ecosystem: local, cloud, external (Claude, Codex, Gemini, Grok,
DeepSeek, Qwen and others) and the Susi harness itself. `.agents/identity.json`
records it as Mandate 74. It builds on Mandates 62–73 (`docs/ceo-agent-mandate.md`)
and wins where older text differs. `intellibitz/susi-commercial-apps` carries the
paired mandate, adapted for closed-source corporate licensing, in its own
`docs/ceo-mandate-74.md`.

The CEO gives the vision, never looks at code, and delegates full autonomous CTO
authority to agents to sharpen, harmonize and execute these mandates end-to-end.
Requirements here are not claims that every target is already met; section 10
states current implementation truth vs. active engineering work.

**Agent reminder (one line):** Follow CEO Mandate 74 (`docs/ceo-mandate-74.md`):
work in your own isolated worktree with remote pull and merge at every step,
keep Susi core build+test+release under 10 seconds across its 5 Apache-2.0
pillars (`GAWD`, `GEMI`, `GMCP`, `ACP`, `Susi Harness`), dump labelled work
straight into `main` with no gates and no PRs, let Susi distill releases with
conflict-free parallel picks, harvest Apache-2.0 code with full attribution, keep
all add-ons closed/licensed/paid in `susi-commercial-apps`, and treat GitHub as a
storage bucket only.

## 0. The 10 Binding CEO Directives (2026-11-11 CTO Codification)

1. **Blazing-fast `<10s` Susi core pipeline:** Susi core must build, test and
   produce its distilled release artifact + SHA-256 provenance manifest in **under
   10 seconds (`< 10.0s` / `total_ms < 10_000` warm pipeline budget)**, recorded
   with structured timing breakdown (`build_ms`, `test_ms`, `release_ms`,
   `total_ms`) and durable evidence while reporting cold vs. warm timings
   separately and honestly (Mandates 1, 62, 69). This budget is the warm fast
   check (`cargo fast-release`: incremental build, the fast test set, the
   distilled artifact and its manifest). It is not the full workspace suite,
   which takes minutes and is what qualifies a release under the exact-tree
   gates. Report both, never one as the other.
2. **Continuous `main` development, deterministic release distillation:** Features
   and fixes land continuously on `main` as an append-only, ungated development
   stream and are deterministically **distilled into qualified immutable releases**
   (`releases/<tag>.json`, `.agents/releases/<name>.json` on
   `refs/candidates/release/<name>`, and immutable release tags).
3. **All add-ons live exclusively in Susi Commercial Apps:** Every add-on feature,
   enterprise pack, SSO/RBAC gateway, FinOps governance plane, multi-host fleet
   controller, visual interface (all UIs and visual views live in Susi Commercial
   Apps), and turnkey corporate workflow lives **exclusively** in
   `intellibitz/susi-commercial-apps` and connects to Susi core over public
   standard socket protocols.
4. **Remote pull and merge at every workflow step:** Every native workflow step
   (`start`, `sync` / `step`, `commit`, `distill`, `ship`, `release`, and parallel
   Susi Harness worker steps) must support **remote pull and merge reconciliation**
   (`git fetch` from `origin` plus clean fast-forward or merge-tree reconciliation
   with `origin/main` or the tracked remote ref when reachable, with safe offline
   operation when disconnected) so distributed agents and isolated laptops stay
   continuously reconciled.
   **Every agent must work on the latest remote** (tightened 2026-11-12): fetch
   and merge `origin/main` before starting work, when resuming work, and before
   every commit, so no agent builds on a stale copy and finds out only when
   publishing. `susi-commercial workflow fresh` reports how far a worktree is
   behind; the commit guard prints a note when it is behind at all and refuses
   the commit beyond a limit (20 commits; an unreachable remote never
   refuses). A merge is never refused: it is the remedy. At start and resume
   the tool pulls for you: `susi-commercial workflow check` and
   `workflow fresh --pull` merge `origin/main` into a worktree that has no
   uncommitted changes, and never touch one that has.
5. **GitHub is portable Git storage only:** `origin` for both `intellibitz/susi`
   and `intellibitz/susi-commercial-apps` is pure Git transport storage — zero
   GitHub Actions, zero hosted PRs or status checks, zero `gh` CLI or GitHub
   REST/GraphQL API lock-in. Moving clouds is a single Git remote URL change.
6. **Constitutional continuity (Mandates 62–74):** Existing CEO mandates
   (Mandates 62–73, `docs/ceo-agent-mandate.md`, `.agents/identity.json`) govern
   the Susi AI ecosystem alongside Mandate 74.
7. **Harmonized CTO governance:** Agents exercise autonomous CTO authority to
   keep `AGENTS.md`, `CLAUDE.md`, `GEMINI.md`, `VISION.md`, `ARCHITECTURE.md`,
   `README.md`, `docs/ceo-agent-mandate.md`, `docs/lean-core.md`, and
   `.agents/identity.json` synchronized without drift.
8. **Susi Core (`intellibitz/susi`) is Apache-2.0 and free forever, built on 5
   primary core pillars:**
   - **GAWD** (`susi-gawd*` — evidence-gated swarm & A2A coordination)
   - **GEMI** (`susi-gemi*` — local & cloud model inference router)
   - **GMCP** (`susi-gmcp` — MCP protocol server & tool plane)
   - **ACP** (`susi-core::acp` — Agent Client Protocol / Agent
     Communication Protocol open standard socket: JSON-RPC/HTTP session, agent
     discovery & capabilities, prompt/turn streaming & cancellation, capability
     admission & conformance)
   - **Susi Harness** (`susi-harness` — driver & engine with built-in parallel
     multi-agent workflow, isolated worktrees/targets/`SUSI_HOME`, capacity
     admission, and serialized integration)
9. **Susi Commercial Apps (`intellibitz/susi-commercial-apps`) is closed,
   licensed, and paid corporate software:** Strictly proprietary, licensed
   (`SUSI-ENT-v1` / Ed25519 `SUSI-CUST-v1`), and paid software built for
   commercial entities and corporates. Every Apache-2.0 repository in any cloud
   may be harvested under strict Apache-2.0 attribution; commercial code never
   flows into public core.
10. **Full autonomous CTO execution & safety:** Zero code inspection by the CEO,
    strict Rust memory and panic safety (`#![forbid(unsafe_code)]` or
    `#![deny(unsafe_code)]` with per-item `// SAFETY:`, zero production
    `unwrap`/`expect`/`panic`/`todo`/`unimplemented`/`unreachable`),
    permissive-only dependency closure (`Apache-2.0`, `MIT`, `BSD-2-Clause`,
    `BSD-3-Clause`), durable evidence, health-checked atomic promotion, and
    verified rollback.

## 1. Model: trunk as a dump, releases as distillates, <10s core loop

This is trunk-based development with release lines cut and distilled from trunk,
with gatekeeping on `main` replaced by deterministic distillation and a blazing-fast
`<10s` warm core verification loop:

| Project | Trunk | How a release is built from it |
| --- | --- | --- |
| Linux kernel | `linux-next` merges hundreds of maintainer trees every day | Stable trees take fixes only after they are in mainline ("upstream first"); each backport cites the mainline commit |
| Chromium | everything lands on `main` | A branch is cut about every 4 weeks; later fixes reach it only as cherry-picks of commits already on `main` |
| Rust | `master` builds nightly | `beta` is branched every 6 weeks; fixes are backported by nomination |
| Firefox | `mozilla-central` | Code moves to `beta` and `release`; fixes reach them as "uplifts" |
| Kubernetes | `master` | `release-1.x` branches receive cherry-picks of merged changes |
| OpenStack Zuul, GitLab merge trains, bors | the target branch | The exact combined tree is tested speculatively before the ref moves |

What Susi takes from these: **land on trunk first, release by selection, cite
the source commit, test the exact combined tree before a release ref moves.**
Susi's difference: there is no human review and no hosted CI. Agents reconcile
with `origin` via remote pull and merge at every step, dump labelled work freely,
and keep the warm core **build + test + release** loop under **10.0 seconds**
(`build_ms + test_ms + release_ms = total_ms < 10_000`) by unifying Cargo
profiles, fast native linking, and single-pass artifact + SHA-256 manifest
generation. Cold clean-target timings are always recorded separately from warm
incremental timings (Mandate 1).

## 2. Main is the dump: remote pull & merge every step, no gates, no workflows

- **Isolation.** One agent, one folder: `git worktree add` (or
  `cargo local-workflow start`) on a private branch, a private `CARGO_TARGET_DIR`,
  a private `SUSI_HOME`, private ports and a unique identity. Never edit the
  primary checkout and never share a writable target. Mandate 73's
  capacity-based admission still applies: no fixed agent-count limit.
- **Remote pull and merge at every step.** Distributed agents across laptops and
  cloud sessions stay synchronized because every workflow lifecycle step
  (`start`, `sync` / `step`, `commit`, `distill`, `ship`, `release`, and parallel
  Susi Harness steps) supports fetching `origin` and reconciling (`pull` /
  fast-forward / `git merge-tree --write-tree` merge) against `origin/main` or
  the tracked remote ref whenever `origin` is reachable, falling back cleanly to
  offline local Git state when disconnected (Mandate 66).
- **Dump.** When a piece of work is ready, publish it to `main` immediately.
  Use `local-snapshot submit` (see `docs/git-storage-publisher.md`) or the
  remote-reconciling workflow step, which fetches `main`, combines the two
  histories with `git merge-tree --write-tree` without touching the worktree,
  creates the merge with `git commit-tree`, and pushes a normal fast-forward. If
  another agent got there first, it recomposes and retries. A plain
  `git fetch && git rebase origin/main && git push` or fast-forward merge is
  also allowed.
- **No gates.** No required tests, reviews, approvals, pull requests, status
  checks, queues or CI before `main`. Run the focused checks that help you, report
  their results honestly, and never make them a hidden prerequisite. Hooks on
  `main` enforce transport safety only: no force-push and no deletion.
- **Main may be red.** `main` is unqualified. Agents fix forward with new commits.
  Susi may publish advisory health records (for example the newest `main`
  commit that built). These are information for agents who want a stable base.
  They are not gates.
- **Append only.** Never rewrite, force-push or reset shared history. Undo is a
  new revert commit.
- **The one obligation: be distillable.** Speed is free; unlabelled work is not.
  Every dump must be selectable later:
  - one feature or fix per commit series, no drive-by mixing of unrelated changes;
  - Git trailers on every commit (`git interpret-trailers`): `Task: T-<AGENT>-<n>`
    and, where it applies, `Feature: <feature-id>`, `Depends: <task-id>`,
    `Harvest: H-<AGENT>-<n>`, `Origin: <url>@<sha>`;
  - a task record under `.agents/tasks/` with its scopes, dependencies and
    acceptance command (`distill lint` checks labelling).
  This is labelling, not a gate: an unlabelled commit still lands. It just
  cannot be distilled into a release until someone labels it.

## 3. Releases: Susi-managed distillation, parallel and conflict-free

GitHub never merges, never squashes and never hosts a pull request for Susi.
All merges, picks, remote pull-and-merge reconciliations and release moves are
pure Git operations run by Susi or agents on a host, then pushed as normal
fast-forwards.

### 3.1 Release state lives in Git

The tools are `local-distill`, `release-lanes` (`docs/release-lanes.md`), and
`local-release` (`docs/releases.md`). The repository's `push-policy` hook
enforces the namespaces below.

- `releases/<tag>.json` and `.agents/releases/<name>.json`: committed release
  provenance and distillation manifests (`susi-release-lanes-v1`) binding
  selected `Task:` commits, dependency closures, source tree OID, lockfile,
  toolchain, timing evidence and SHA-256 artifact digests.
- `refs/candidates/release/<name>`: the release line. It is not a branch under
  `refs/heads/*`, so a Git host never shows it in branch lists, never offers a
  pull request for it and never shows a merge button. `push-policy` admits only
  fast-forwards there and refuses deletion.
- `refs/claims/release-lanes/<name>/<lane>`: one claim per lane, a small JSON
  blob naming the agent. Git refuses to replace an existing non-commit ref
  without force, and the receiving side compares the old value atomically. That
  makes the claim an atomic create-if-absent, with no lock server.
- `refs/tags/<version>` and `refs/tags/susi-distill/<name>`: immutable snapshots
  (Mandate 73). Never moved or reused. `push-policy` admits a `vX.Y.Z` tag only
  with its exact-tree qualification record.

### 3.2 How many agents advance one release without conflicts

1. **Plan (`release-lanes init` / `local-distill plan`).** Pull/reconcile latest
   `origin` refs, select features from `main` by their `Task:` trailers and task
   records, and compute the transitive dependency closure, rejecting cycles.
   `init` splits the result into **lanes with disjoint file scopes**. Tasks whose
   paths overlap, or that depend on each other, share one lane in dependency
   order. Lanes are what make parallel work conflict-free.
2. **Claim (`claim`).** Each agent claims one lane. A stale claim can be taken
   over only past an explicit age (`--stale-secs`).
3. **Pick in private (`pick`).** From the current release tip (reconciled with
   `origin`), each source commit is cherry-picked in memory, the way
   `git cherry-pick -x` does: its `(cherry picked from commit ...)` line records
   the source, as the stable kernel does. Commits already present (by that line
   or by `git patch-id --stable`) are skipped. `git merge-tree --write-tree`
   does the merge without touching any checkout. A conflict stops with the file
   list; the agent resolves it in its own worktree and resumes with `pick --resume`.
4. **Qualify the exact tree (`verify`).** Release lines keep the
   full exact-tree gates from Mandate 73: license and Rust-only audits, format,
   clippy, non-empty tests and executable smoke tests on tip + picks. `verify`
   runs the given command in a fresh worktree of the exact candidate and records
   evidence bound to its tree.
5. **Advance (`advance`).** One `git push --atomic` moves the release line by a
   normal fast-forward (code plus the manifest update) and deletes the claim, so
   both succeed or neither does. The server compares the old value atomically.
   Only one agent wins a given tip, and nobody is overwritten.
6. **Lose the race, re-pick (`ship`).** If the tip moved, `advance` refuses the
   stale candidate. `ship` loops remote pull/reconcile, pick, verify and advance
   with a bounded number of attempts, re-qualifying every new tree. Evidence is
   never reused across trees. After the last attempt the claim stays with the
   agent, who can retry or `unclaim`. Never force.
7. **Upstream first.** A fix for a release goes to `main` first, then is picked.
   A fix that only makes sense on the release line is allowed, but it must be
   recorded as its own task with the reason.
8. **Snapshot.** When every lane is `qualified`, `release-lanes status` names the
   candidate commit. `local-release qualify` cuts the immutable `vX.Y.Z` tag and
   `releases/<tag>.json` record on that exact commit, bound to the tree,
   lockfile, toolchain, test results and artifact hashes.

### 3.3 No PR prompts and no branch clutter on the storage remote

GitHub offers "Compare & pull request" for recently pushed branches under
`refs/heads/*`. So:

- Release lines, claims and candidates live outside `refs/heads`, so no prompt
  or merge button appears for them.
- `push-policy` admits only `refs/heads/main` as a remote branch. Agent work
  branches (`local-agent/<agent>`) stay local; an agent publishes or backs up
  its work to the non-branch ref `refs/agents/<agent>`, which GitHub never offers
  as a pull request (`docs/agent-refs.md`). Existing remote `local-agent/*`
  branches may only be deleted, after their tip is preserved there.
- Agents never call `gh` pull-request commands, the GitHub REST/GraphQL API,
  auto-merge or merge queues, and never change repository settings, branch
  protection or rulesets. Server-side transport protection (non-fast-forward and
  deletion rules) is storage safety and stays as it is.
- **Storage safety has zero hosting-provider dependency (CEO decision
  2026-11-11).** Neither repository has a host ruleset or branch protection
  (verified 2026-11-11), and none is added: a rule that exists only on one
  provider would not move with the repository. The two portable controls are
  `push-policy`, switched on by a clone's first build, and the storage guard
  (`local-snapshot guard`, `docs/git-storage-publisher.md`), with which every
  clone witnesses that `main` and the release lines only moved forward and that
  no tag moved or vanished, and keeps what it saw so a loss is restored without
  force. A remote the CEO controls can add Git's own
  `receive.denyNonFastForwards` and `receive.denyDeletes`; those are Git, not
  a provider feature.

## 4. GitHub is a storage bucket; Git is the source manager

- **Allowed GitHub use:** Git transport only (`fetch`, `pull`/`merge`, and `push`
  over HTTPS or SSH).
- **Forbidden as dependencies:** GitHub Actions, pull requests, issues or
  projects as state, required checks, GitHub Releases as the only artifact
  store, Packages, LFS, webhooks, Dependabot, CODEOWNERS enforcement, merge
  queues, the REST/GraphQL API and `gh` CLI in any tooling or runtime path.
- **All coordination state is Git data:** tasks, claims, evidence, receipts,
  release records (`releases/<tag>.json`), lane manifests
  (`.agents/releases/<name>.json`), harvest manifests and notes are files on
  `main` or on the release lines, or refs under `refs/agents/*`,
  `refs/candidates/*`, `refs/claims/*`, `refs/claim-agents/*`,
  `refs/claim-generation/*` and `refs/closed/*` (the namespaces `push-policy`
  admits). Nothing exists only on a hosting provider.
- **Provider neutrality is checked:** a hardcoded `github.com` URL in code or
  tooling is a defect (`tests/no_github_coupling.rs`). URLs in documentation,
  attribution and harvest manifests are fine. The storage remote is configuration
  (`origin` plus optional mirrors).
- **Moving clouds is one operation:** add the new remote, then push
  `refs/heads/*`, `refs/tags/*` and those coordination namespaces without force
  (`git push --mirror` is not used because it also deletes refs). Then
  `git remote set-url origin <new>`. No code changes.
- **Portability drill (to build):** Susi periodically clones from a secondary
  remote into scratch, runs `git fsck`, compares every ref with `origin` and
  builds the result. Cold backups are `git bundle create --all` files checked
  with `git bundle verify` and kept in any object store or on disk.
- Agents do not create hosting accounts or change credentials or access. The CEO
  provisions remotes, and agents use the ones that exist.

## 5. Harvest: the open-source world is the oyster

Any public Git repository on any host (GitHub, GitLab, Codeberg, SourceHut,
Gitee, Bitbucket, self-hosted) is a source when the code Susi takes is
licensed **Apache-2.0**.

**Apache-2.0 only (CEO decision 2026-11-12, "only apache 2.0 license only for
pure compliance").** Harvested code is taken under Apache-2.0 and nothing else.
A dual-licensed file (`Apache-2.0 OR MIT`) is taken under its Apache-2.0
alternative and recorded as Apache-2.0. Code that is only MIT, BSD-2-Clause or
BSD-3-Clause, `Apache-2.0 WITH LLVM-exception`, or Apache-2.0 combined with
another license (`AND`) is not harvested; `local-harvest` and the distillation
audit refuse it. This rule is for source copied into Susi. The allowlist for
linked dependencies (Mandate 68) is a separate rule and is unchanged.

**Why harvest (the CEO's reasoning, 2026-11-12).** Both repositories work the
same way: raw source on `main`, distilled releases. Susi picks up features that
already exist in the open-source world and integrates them into core and into
Susi Commercial Apps. Core stays lean and efficient because it does not
reinvent what exists and takes only what it needs; Commercial Apps gets
innovative functionality that is already there. One license keeps compliance
simple enough to verify for every file.

### 5.1 License verification at the exact commit

- Check the license **at the pinned commit, for each harvested file**, not just
  the repository's front page: the root `LICENSE`, SPDX headers in each file,
  `NOTICE`, and any `third_party/` or vendored directories, which often carry
  other licenses.
- Reject: GPL, LGPL, AGPL, MPL, EPL, CDDL, SSPL, BUSL/BSL, Elastic, Commons
  Clause, "source-available", non-commercial and custom licenses, and anything
  with no license. If in doubt, do not take it.
- Never take trademarks, logos or product names (Apache-2.0 section 6).

### 5.2 Intake mechanics

The tool is `local-harvest` (`docs/harvest.md`): `intake`, `scan`, `import`,
`notices`, `audit`, `raw-features`, `route` and `pickup` implement the steps
below under [CEO Mandate 75](ceo-mandate-75.md).

1. **Quarantine fetch:** `git fetch --no-tags <url> <sha>:refs/susi/upstream/<slug>/<sha>`.
   Upstream code is untrusted. During intake, never run its build scripts,
   hooks, tests or binaries. Read it first, compile it only inside Susi's tree
   and only after review.
2. **Choose the import mode:** cherry-pick with `-x` when the histories are related;
   `git format-patch` / `git am` to keep authorship of file-level patches; a
   pinned source transplant; or a Rust port (Mandate 63: non-Rust code must be
   ported, and a translation is still a derivative work that needs attribution).
3. **Harvest manifest** `.agents/harvest/H-<AGENT>-<n>.json`
   (`susi-harvest-manifest-v1`): source URL, full commit SHA, source paths,
   verified SPDX license, hashes of LICENSE and NOTICE at that commit, copyright
   holders, NOTICE text, import mode, destination paths, a summary of
   modifications, the agent and the date, plus the linked task and feature.
4. **Apache-2.0 section 4 obligations, every time:**
   - (a) ship the license text (`LICENSES/Apache-2.0.txt`);
   - (b) put a prominent notice in every modified file saying it was changed
     (for example `// Modified by Susi contributors, 2026-11-11: <summary>`);
   - (c) keep every upstream copyright, patent, trademark and attribution notice;
   - (d) add the upstream NOTICE attributions to Susi's `NOTICE` /
     `THIRD_PARTY_NOTICES`, which ship with every distribution.
5. **Commit trailers:** `Harvest: H-<AGENT>-<n>`, `Origin: <url>@<sha>:<path>`,
   `Origin-License: Apache-2.0`.
6. **Dump, distill, release** like any other feature. Release qualification
   covers the harvested files: `cargo deny` only sees crates, so the release
   license audit must also check that every file with an `Origin` has its
   manifest and notice (section 10).
7. **Prefer harvest over new dependencies** when it reduces dependency reach and
   size (Mandate 69 and the lean-core ladder). Harvested code is then Susi's to maintain.

### 5.3 Giving back

Susi core is Apache-2.0, so others may harvest from it. Fixes to harvested code
should go upstream. Every upstream contribution follows that project's
rules (DCO or CLA). Signing a CLA, or contributing to an organization for the
first time, is an outward legal act: it needs CEO authorization. Commercial code,
credentials and private plans never go upstream.

## 6. Susi Core (5 Pillars, Apache-2.0 Free Forever) vs. Susi Commercial Apps (Closed, Licensed, Paid)

- **Susi Core (`intellibitz/susi`)** is Apache-2.0 and free forever, engineered
  for a `<10s` warm build+test+release cycle and organized strictly around **5
  primary core pillars**:
  1. **GAWD**: evidence-gated multi-agent swarm & A2A coordination;
  2. **GEMI**: local & cloud model inference router with auditable placement;
  3. **GMCP**: MCP protocol server & governed tool plane;
  4. **ACP**: Agent Client Protocol / Agent Communication Protocol standard open
     socket (session lifecycle, agent capability discovery, streaming turns &
     cancellation, capability admission & conformance);
  5. **Susi Harness**: recursive driver & engine with built-in parallel
     multi-agent workflow, isolated worktrees/targets/`SUSI_HOME`, capacity
     admission and serialized integration.
  Third-party product implementations, visual views and add-on packs stay out of
  core (`AGENTS.md`, open-core boundary; all visual views and curated plugin
  packs live in Susi Commercial Apps).
- **Susi Commercial Apps (`intellibitz/susi-commercial-apps`)** is strictly
  closed-source, proprietary, cryptographically licensed (`SUSI-ENT-v1` and
  offline Ed25519 `SUSI-CUST-v1`), and paid software for commercial entities and
  corporates. It follows this mandate in its own private repository (remote pull
  and merge on every step, ungated `main` dump, deterministic feature
  distillation into releases, pure Git storage). It may harvest Apache-2.0 code
  from anywhere, including Susi core, with full attribution. Code never flows from commercial into core.
- **Exclusive commercial add-on scope:** Every add-on feature, enterprise ACP
  agent bridge, multi-user SSO/RBAC gateway, corporate fleet governance plane,
  FinOps cost control, pre-trained reflex store bundle, certified hardware binary
  and desktop/operator visual interface (all UIs and visual views live in Susi
  Commercial Apps) is developed and shipped exclusively in
  `susi-commercial-apps`, connecting over Susi core's public protocol socket.

## 7. The harness: driver, engine, and parallel multi-agent workflow

- **Today, both directions:** external agents drive and use the Susi harness as
  their engine (isolated worktrees, remote pull & merge at every step, claims,
  `<10s` fast-release verification, dump, distill, publish). Conversely, the
  Susi harness drives multiple agents in parallel as workers over **ACP**, MCP,
  A2A, OpenAI-compatible HTTP and Anthropic Messages. Mandate 73's recursive
  driver/engine rules apply: capability contracts, budgets, cycle checks and no
  permission amplification.
- **BYOK:** any provider, with keys the user brings. Keys are resolved from the OS
  credential store or the configured KeePassXC automation vault, never from the
  repository, logs or prompts. Discovery needs no configuration (Mandate 70).
- **Becoming the default driver is earned, not declared:** the harness becomes
  the default driver when it matches the reference frontier agents (Opus,
  Astra, Carbon level) on a fixed Susi task suite (real backlog tasks with
  acceptance commands). It is compared on acceptance pass rate, regressions,
  wall time and cost, over repeated runs, with recorded evidence (Mandate 71).
  Until then it never blocks an external driver.

## 8. What does not change

No force-push or history rewrite on shared refs. Release branches and tags keep
exact-tree qualification and immutable provenance. Rust-only (63), native Windows
(64), strict memory & panic safety (59: `forbid`/`deny(unsafe_code)`, zero
production panics), the permissive license allowlist (68: `Apache-2.0`, `MIT`,
`BSD-2-Clause`, `BSD-3-Clause`), lean core ratchets (69), confidentiality between
the two repositories, credential handling, spending and destructive-action
limits, and live access controls are unchanged. This mandate authorizes no
changes to repository settings, branch protection, accounts or credentials.

## 9. Every agent, every session

1. Read `AGENTS.md`, this mandate and Mandate 73. Fetch and inspect the actual
   `origin/main` head.
2. Create your own isolated worktree, branch, private `CARGO_TARGET_DIR`, private
   `SUSI_HOME` and unique identity. Claim disjoint scopes.
3. Reconcile via remote pull and merge across workflow steps (`start`, `sync`,
   `commit`, `distill`, `ship`, `release`) whenever `origin` is reachable.
4. Build the core pillar capability or fix yourself (keeping core inside the 5
   Apache-2.0 pillars and the `<10s` warm build+test+release budget; routing all
   add-ons to `susi-commercial-apps`), or harvest Apache-2.0 code with manifest,
   notices and trailers.
5. Label every commit (`Task: T-<AGENT>-<n>` trailer and `.agents/tasks/` record)
   and dump to `main` via fast-forward without PRs or hosted gates.
6. Distill and release only through Susi's lanes, claims, picks, `releases/<tag>.json`
   records and exact-tree qualification. Never through GitHub.
7. Report honestly in CEO language: measured timings (cold vs. warm), what landed
   (verified in `origin` history), what is unqualified, and what is blocked.

## 10. Implementation truth (2026-11-11)

Exists: isolated worktrees and claims (`cargo local-workflow`), non-gated main
submission (`local-snapshot submit`), plan/compose/orchestrate distillation
with feature manifests (`local-distill`, `docs/release-distillation.md`), the
`distill lint` labelling check, recursive driver/engine delegation, `release-lanes`
(`docs/release-lanes.md`) lane partitioning by disjoint scopes with atomic lane
claims and in-memory `-x` picks + `git patch-id --stable` dedup, and
`local-harvest` (`docs/harvest.md`) quarantine intake, per-file SPDX scans,
`susi-harvest-manifest-v1` imports, `THIRD_PARTY_NOTICES` generation and
Apache-2.0 section 4 audits enforced on release gates (`T-CARBONDISTILL-2`).

Active 2026-11-11 CTO wave deliverables (verified independently before claiming
completion per Mandate 1): unified `<10s` warm core `build + test + release`
pipeline (`fast-release` timing breakdown `build_ms`, `test_ms`, `release_ms`,
`total_ms < 10_000` with durable evidence), first-class **ACP** 5th core pillar
socket admission & conformance alongside parallel Susi Harness workflow, remote
pull-and-merge reconciliation wired across every workflow step in both `susi` and
`susi-commercial-apps`, and enterprise ACP/corporate add-on packs in
`susi-commercial-apps`.

Since 2026-11-11 (`T-CLAUDESTORAGEGUARD-1`): the storage guard
(`local-snapshot guard`) and first-build hook enabling (root `build.rs`).

Since `T-CLAUDESTORAGEGUARD-2` every `local-snapshot` publish runs the guard
afterwards, so each dump to main witnesses the remote without a scheduler.

Since `T-CLAUDESTORAGEGUARD-3` the harness's `submit-main`, `integrate` and
`release-promote` do the same, and since `T-CLAUDESTORAGEGUARD-4` so does
`local-workflow ship`, in core and in `susi-commercial-apps`.

Still open beyond this wave: speculative-stack qualification and safe evidence
reuse across trees; claim heartbeats; advisory `main` health records; the
secondary-mirror portability drill and bundle backups; and the multi-run harness
parity evaluation for default-driver promotion.
