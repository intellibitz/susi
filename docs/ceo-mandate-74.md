# CEO Mandate 74: dump to main, distill releases, harvest the open-source world

Effective 2026-11-11. Binding on every agent that touches `intellibitz/susi`:
local, cloud, external (Claude, Codex, Gemini, Grok, DeepSeek, Qwen and others)
and the Susi harness itself. `.agents/identity.json` records it as Mandate 74.
It builds on Mandate 73 (`docs/ceo-agent-mandate.md`) and wins where the two
differ. `susi-commercial-apps` carries the same mandate, adapted for closed
source, in its own `docs/ceo-mandate-74.md`.

The CEO gives the vision and never looks at code. This document is the agents'
translation of that vision into engineering rules. Requirements here are not
claims that the tooling already exists; section 10 says what exists and what is
still to build.

**Agent reminder (one line):** Follow CEO Mandate 74 (`docs/ceo-mandate-74.md`):
work in your own isolated worktree, dump labelled work straight into `main` with
no gates and no PRs, let Susi distill releases with conflict-free parallel
picks, harvest Apache-2.0 code with full attribution, and treat GitHub as a
storage bucket only.

## 0. The CEO's vision

1. Development is lightning fast. Every agent works in an isolated local folder
   and dumps features and fixes into `main` fast. Speed is the key. No gates,
   no workflows. `main` becomes a feature dump, and releases are distilled from it.
2. Release branches are managed by Susi, not by GitHub. GitHub never merges and
   never prompts for pull requests. Many agents move one release forward in
   parallel without conflicts.
3. The CEO gives only the vision. Agents translate it to the highest technical level.
4. Every Apache-2.0 Git repository in any cloud is available to Susi. Agents
   cherry-pick functionality into `main`, then distill and release it. Susi core
   contributes to the open-source world and takes from it.
5. Susi Commercial Apps is strictly licensed and closed source, and all of the
   above applies to it too, with Apache-2.0 license terms followed strictly.
6. Git is the source code manager. GitHub is a storage bucket and nothing more.
   The Susi ecosystem can move to another cloud at any time, with zero
   dependency on GitHub.
7. The Susi harness is both driver and engine: it uses agents, and agents use it.
   It becomes the default driver when it can plug in any agent with BYOK
   seamlessly and performs at the level of Opus, Astra and Carbon.
8. Susi core (Apache-2.0, open-standards protocol AI OS socket with GAWD, GEMI,
   GMCP, ACP and the Susi harness with its embedded parallel workflow) is free
   forever. Susi Commercial Apps is closed source and licensed, and always
   carries extra functionality that companies pay for.

## 1. Model: trunk as a dump, releases as distillates

This is trunk-based development with release branches cut from the trunk,
and with gatekeeping replaced by distillation. Large projects already work this way:

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
Susi's difference: there is no human review and no hosted CI. Agents dump
freely. Susi's distiller selects, composes and qualifies.

## 2. Main is the dump: no gates, no workflows

- **Isolation.** One agent, one folder: `git worktree add` (or
  `cargo local-workflow start`) on a private branch, a private `CARGO_TARGET_DIR`,
  a private `SUSI_HOME`, private ports and a unique identity. Never edit the
  primary checkout and never share a writable target. Mandate 73's
  capacity-based admission still applies: no fixed agent-count limit.
- **Dump.** When a piece of work is ready, publish it to `main` immediately.
  Use `local-snapshot submit` (see `docs/git-storage-publisher.md`), which fetches
  `main`, combines the two histories with `git merge-tree --write-tree` without
  touching the worktree, creates the merge with `git commit-tree`, and pushes
  a normal fast-forward. If another agent got there first, it recomposes and
  retries. A plain `git fetch && git rebase origin/main && git push` is also allowed.
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

## 3. Releases: Susi-managed, parallel and conflict-free

GitHub never merges, never squashes and never hosts a pull request for Susi.
All merges, picks and release moves are Git operations run by Susi or agents
on a host, then pushed as normal fast-forwards.

### 3.1 Release state lives in Git

The tool is `release-lanes` (`docs/release-lanes.md`). The repository's
`push-policy` hook enforces the namespaces below.

- `refs/candidates/release/<name>`: the release line. It is not a branch, so a
  Git host never shows it in branch lists, never offers a pull request for it
  and never shows a merge button. `push-policy` admits only fast-forwards there
  and refuses deletion.
- `.agents/releases/<name>.json` on the release line: the release manifest
  (`susi-release-lanes-v1`). It records the pinned base, the lanes with their
  tasks, source commits and status (`pending`, `qualified`), and for each
  advance the agent, the picks, the qualified tree and the evidence digest.
- `refs/claims/release-lanes/<name>/<lane>`: one claim per lane, a small JSON
  blob naming the agent. Git refuses to replace an existing non-commit ref
  without force, and the receiving side compares the old value atomically. That
  makes the claim an atomic create-if-absent, with no lock server.
- `refs/tags/<version>`: immutable snapshots (Mandate 73). Never moved or reused.
  `push-policy` admits a `vX.Y.Z` tag only with its exact-tree qualification record.

### 3.2 How many agents advance one release without conflicts

1. **Plan (`release-lanes init`).** `local-distill plan` selects features from
   `main` by their trailers and task records and computes the transitive
   dependency closure, rejecting cycles. `init` splits the result into **lanes
   with disjoint file scopes**. Tasks whose paths overlap, or that depend on each
   other, share one lane in dependency order. Lanes are what make parallel work
   conflict-free.
2. **Claim (`claim`).** Each agent claims one lane. A stale claim can be taken
   over only past an explicit age (`--stale-secs`).
3. **Pick in private (`pick`).** From the current release tip, each source commit
   is cherry-picked in memory, the way `git cherry-pick -x` does: its
   `(cherry picked from commit ...)` line records the source, as the stable
   kernel does. Commits already present (by that line or by
   `git patch-id --stable`) are skipped. `git merge-tree --write-tree` does the
   merge without touching any checkout. A conflict stops with the file list; the
   agent resolves it in its own worktree and resumes with `pick --resume`.
4. **Qualify the exact tree (`verify`).** Release lines keep the
   full exact-tree gates from Mandate 73: license and Rust-only audits, format, clippy, non-empty
   tests and executable smoke tests on tip + picks. `verify` runs the given
   command in a fresh worktree of the exact candidate and records evidence bound
   to its tree.
5. **Advance (`advance`).** One `git push --atomic` moves the release line by a
   normal fast-forward (code plus the manifest update) and deletes the claim, so
   both succeed or neither does. The server compares the old value atomically.
   Only one agent wins a given tip, and nobody is overwritten.
6. **Lose the race, re-pick (`ship`).** If the tip moved, `advance` refuses the
   stale candidate. `ship` loops pick, verify and advance with a bounded number of
   attempts, re-qualifying every new tree. Evidence is never reused across trees.
   After the last attempt the claim stays with the agent, who can retry or
   `unclaim`. Never force.
7. **Upstream first.** A fix for a release goes to `main` first, then is picked.
   A fix that only makes sense on the release line is allowed, but it must be
   recorded as its own task with the reason.
8. **Snapshot.** When every lane is `qualified`, `release-lanes status` names the
   candidate commit. `local-release qualify` cuts the immutable `vX.Y.Z` tag on
   that exact commit, bound to the tree, lockfile, toolchain, test results and
   artifact hashes.

### 3.3 No PR prompts and no branch clutter on the storage remote

GitHub offers "Compare & pull request" for recently pushed branches under
`refs/heads/*`. So:

- Release lines, claims and candidates live outside `refs/heads`, so no prompt
  or merge button appears for them.
- `push-policy` admits only `refs/heads/main` and `refs/heads/local-agent/<agent>`
  as remote branches. Agent work branches stay local. A `local-agent/<agent>`
  backup is allowed, but GitHub lists it as a branch, so use it only when off-host
  backup is needed.
- Agents never call `gh` pull-request commands, the GitHub API, auto-merge or merge queues, and
  never change repository settings, branch protection or rulesets. Server-side
  transport protection (non-fast-forward and deletion rules) is storage safety
  and stays as it is.

## 4. GitHub is a storage bucket; Git is the source manager

- **Allowed GitHub use:** Git transport only (`fetch` and `push` over HTTPS or SSH).
- **Forbidden as dependencies:** GitHub Actions, pull requests, issues or
  projects as state, required checks, GitHub Releases as the only artifact
  store, Packages, LFS, webhooks, Dependabot, CODEOWNERS enforcement, merge
  queues, the REST/GraphQL API and `gh` in any tooling or runtime path.
- **All coordination state is Git data:** tasks, claims, evidence, receipts,
  release manifests, harvest manifests and notes are files on `main` or on the
  release lines, or refs under `refs/candidates/*`, `refs/claims/*`,
  `refs/claim-agents/*`, `refs/claim-generation/*` and `refs/closed/*` (the
  namespaces `push-policy` admits). Nothing exists only on a hosting provider.
- **Provider neutrality is checked:** a hardcoded `github.com` URL in code or
  tooling is a defect. URLs in documentation, attribution and harvest manifests
  are fine. The storage remote is configuration (`origin` plus optional mirrors).
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
licensed **Apache-2.0**. That includes `Apache-2.0 OR MIT` and
`Apache-2.0 WITH LLVM-exception`. MIT, BSD-2-Clause and BSD-3-Clause remain
allowed under Mandate 68.

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
`notices` and `audit` implement the steps below.

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

## 6. Susi core and Susi Commercial Apps

- **Susi core** is Apache-2.0 and free forever: the open-standards AI OS socket
  with GAWD, GEMI, GMCP, the ACP surface and the Susi harness with its embedded
  parallel workflow. Product implementations stay out of core (`AGENTS.md`, open-core boundary).
- **Susi Commercial Apps** is closed source, licensed and paid. It follows this
  whole mandate in its own repository. It may take Apache-2.0 code from anywhere,
  including Susi core, with full attribution. Code never flows from commercial into core.
- Commercial always carries paid-only value: organization-scale governance,
  fleet, SSO/RBAC, FinOps, turnkey packs, certified binaries and the desktop.

## 7. The harness: driver and engine

- **Today, both directions:** external agents drive and use the harness as their
  engine (worktrees, claims, dump, distill, publish). The harness drives external
  agents as workers over ACP, MCP, A2A, OpenAI-compatible HTTP and Anthropic
  Messages. Mandate 73's recursive driver/engine rules apply: capability
  contracts, budgets, cycle checks and no permission amplification.
- **BYOK:** any provider, with keys the user brings. Keys are resolved from the OS
  credential store or the configured vault, never from the repository, logs or
  prompts. Discovery needs no configuration (Mandate 70).
- **Becoming the default driver is earned, not declared:** the harness becomes
  the default driver when it matches the reference frontier agents (Opus,
  Astra, Carbon level) on a fixed Susi task suite (real backlog tasks with
  acceptance commands). It is compared on acceptance pass rate, regressions,
  wall time and cost, over repeated runs, with recorded evidence (Mandate 71).
  Until then it never blocks an external driver.

## 8. What does not change

No force-push or history rewrite on shared refs. Release branches and tags keep
exact-tree qualification and immutable provenance. Rust-only (63), native Windows
(64), the license allowlist (68), lean core (69), confidentiality between the two
repositories, credential handling, spending and destructive-action limits, and
live access controls are unchanged. This mandate authorizes no changes to
repository settings, branch protection, accounts or credentials.

## 9. Every agent, every session

1. Read `AGENTS.md`, this mandate and Mandate 73. Check the actual repository head.
2. Create your own worktree, branch, target, home and identity. Claim disjoint scopes.
3. Build the feature yourself, or harvest it (Apache-2.0, manifest, notices, trailers).
4. Label it (task record and trailers), commit, and dump to `main` at once. No PRs.
5. Distill and release only through Susi's lanes, claims, picks and exact-tree
   qualification. Never through GitHub.
6. Report honestly in CEO language: what landed (verified in the remote's
   history), what is unqualified, and what is blocked.

## 10. Implementation truth (2026-11-11)

Exists: isolated worktrees and claims (`cargo local-workflow`), non-gated main
submission (`local-snapshot submit`), plan/compose/orchestrate distillation
with feature manifests (`local-distill`, `docs/release-distillation.md`), the
`distill lint` labelling check, and recursive driver/engine delegation.

Since 2026-11-11, `release-lanes` (`docs/release-lanes.md`) provides lane
partitioning by disjoint scopes, release manifests, atomic lane claims,
in-memory `-x` picks with patch-id dedup, exact-tree evidence, and atomic
fast-forward advancement with a bounded retry loop.
`local-harvest` (`docs/harvest.md`) provides quarantine intake, per-file
license scans, `susi-harvest-manifest-v1` imports, `THIRD_PARTY_NOTICES`
generation, and an audit of Apache-2.0 section 4 (a) to (d) and of `Harvest:`
trailers. Both work in `susi-commercial-apps` too, whose hook admits the same namespaces.

To build (open work, not claims): wiring `local-harvest audit` into
`local-release qualify`; speculative-stack qualification and safe evidence reuse
across trees; claim heartbeats; advisory `main` health records; the mirror
portability drill and bundle backups; the provider-neutrality check for
hardcoded hosts; and the harness parity evaluation for default-driver promotion.
