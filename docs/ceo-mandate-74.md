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
  Susi may publish advisory health refs (for example `refs/susi/health/main`
  pointing to the newest commit that built). These are information for agents
  who want a stable base. They are not gates.
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

- `refs/heads/release/<name>`: the release line. It advances only by fast-forward.
- `.agents/releases/<name>.json` on that branch: the release manifest. It lists
  the pinned base, the candidate features with dependency closure and the status of
  each (`proposed`, `claimed`, `picked`, `qualified`, `rejected` with a reason).
  It also records the `git patch-id --stable` of every pick.
- `refs/susi/claims/release/<name>/<feature-id>`: one claim per feature. To claim,
  push a fresh parentless commit that names the agent identity. Any competing
  claim is unrelated history, so the server rejects it as non-fast-forward. That
  makes the claim an atomic create-if-absent, with no lock server.
- `refs/tags/<version>`: immutable snapshots (Mandate 73). Never moved or reused.

### 3.2 How many agents advance one release without conflicts

1. **Plan (distiller).** `local-distill plan` selects features from `main` by
   their trailers and task records and computes the transitive dependency
   closure. It rejects dependency cycles and splits the work into **lanes with
   disjoint file scopes**. Features whose scopes overlap go into one lane, in
   dependency order. Lanes are what make parallel work conflict-free.
2. **Claim.** Each agent claims the features of one lane (section 3.1).
3. **Pick in private.** In its own worktree, from the current release tip:
   `git cherry-pick -x` each commit (the `-x` line records the source commit,
   as the stable kernel does). First skip anything already present by checking
   `git cherry` and patch-ids. Use `git merge-tree --write-tree` to preview
   conflicts without touching any checkout. Turn on `git rerere`; its cache
   is shared by all worktrees of the clone, so a resolution only has to be made once.
4. **Qualify the exact tree.** Release branches keep the full exact-tree gates
   from Mandate 73: license and Rust-only audits, format, clippy, non-empty tests
   and executable smoke tests on tip + picks. Agents may qualify speculative stacks
   (tip + A, tip + A + B) in parallel, the way Zuul does.
5. **Advance.** Push with `git push --atomic` as a normal fast-forward. One push
   moves the release ref, updates the manifest status and releases the claim, so
   all three succeed or none do. The server compares the old value atomically.
   Only one agent wins a given tip, and nobody is overwritten.
6. **Lose the race, re-base the pick.** If the tip moved, fetch, re-pick onto the
   new tip and re-qualify. Earlier evidence may be reused only if the new commits
   are in disjoint lanes and the affected-package analysis shows the dependency
   closure is unchanged. Otherwise re-run the gates. Retries are bounded. After
   that, hand the lane back with a recorded reason. Never force.
7. **Upstream first.** A fix for a release goes to `main` first, then is picked.
   A fix that only makes sense on the release branch is allowed, but the manifest
   must say why.
8. **Snapshot.** When the manifest is fully `qualified`, tag the release: an
   immutable tag bound to the exact commit and tree, the lockfile, the toolchain,
   test results and artifact hashes.

### 3.3 No PR prompts and no branch clutter on the storage remote

GitHub offers "Compare & pull request" for recently pushed branches under
`refs/heads/*`. So:

- Only `main`, `release/*` and tags live under `refs/heads` and `refs/tags`
  on the storage remote.
- Agent work branches stay local. If they need off-host backup, push them to
  `refs/susi/work/<agent>/<branch>`. Branch listings, PR prompts and the merge
  button never see that namespace. Probe once per remote that it accepts the
  namespace, and record the result. Do not assume.
- Agents never call `gh pr`, the GitHub API, auto-merge or merge queues, and
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
  release manifests, harvest manifests and notes are files on branches or refs
  under `refs/susi/*`. Nothing exists only on a hosting provider.
- **Provider neutrality is checked:** a hardcoded `github.com` URL in code or
  tooling is a defect. URLs in documentation, attribution and harvest manifests
  are fine. The storage remote is configuration (`origin` plus optional mirrors).
- **Moving clouds is one operation:** add the new remote, then push
  `refs/heads/*`, `refs/tags/*` and `refs/susi/*` without force
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

To build (open work, not claims): release manifests and per-feature claim refs
under `refs/susi/claims/release/*`; lane partitioning by disjoint scopes; atomic
multi-ref release advancement with bounded retry; the `refs/susi/work/*` backup
namespace and its per-remote probe; advisory `refs/susi/health/main`;
`susi-harvest-manifest-v1`, quarantine intake and a harvest license audit in
release qualification; `THIRD_PARTY_NOTICES` generation; the mirror portability
drill and bundle backups; the provider-neutrality check for hardcoded hosts; and
the harness parity evaluation for default-driver promotion.
