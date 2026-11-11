# CEO mandate: independent agents, Susi integration and pristine releases

Effective 2026-11-10. This is the operating mandate for every Susi agent, local
or cloud, and the implementation contract for the Susi master brain. It
consolidates the CEO's prior vision with the three-laptop plan. Mandate 73 in
`.agents/identity.json` records it; `AGENTS.md` remains the engineering entry
point. Existing mandates remain binding except the fixed remote push cadence
single-laptop publication restriction and mandatory main-push gates explicitly
superseded below. The CEO's latest direction is: no mandatory gates for main;
full qualification belongs on release branches and release snapshots.
Requirements are not evidence of completed implementation.

## 1. The CEO sets direction; agents deliver

The CEO never looks at code. Agents own discovery, architecture, implementation,
tests, debugging, integration and production qualification. The workflow and
harness are the foundation for Susi to build, operate and improve itself in
successive groups of 100 steps. Step 1 remains open until its measured delivery
and response budgets in `step1-acceptance.md` pass, including a genuine change
reaching the verified live executable in under 60 seconds. Source publication
alone is not a release or proof of improvement.

## 2. Three isolated laptops, independent cloud sessions

Each of the three laptops is an isolated execution host. Each has the four
named agent roles: **dot, opus, carbon and grok**. On each laptop, an
**available, authorized local driver** coordinates the other
local agents and allocates disjoint work. It may be dot, duet, another available
agent, or the Susi harness when its required capabilities are verified. It
tracks evidence, resolves handoffs and reports outcomes. Local driver authority
does not override the CEO's instructions, permissions or another worker's claim.

The cloud sessions of these agents run independently in full autonomous mode
within the CEO's authorized work. They do not wait for a laptop driver to assign
every step. Independence does not allow shared writable state, conflicting
claims, secret sharing or ungated writes. Local and cloud instances use distinct
identities containing host/session, agent and execution mode, plus private
source worktrees, branch names, build outputs, scratch homes and runtime ports.
No worker impersonates another worker or the CEO.

These names express roles and desired deployment, not proof that a product,
model, account or connector is installed, reachable or licensed. Record actual
availability and limitations. A blocked provider does not stop unrelated work.
The four-role plan is not a universal four-worker ceiling. There is **no
arbitrary agent-count limit or fixed push-rate limit**. Admit work according to
measured CPU, RAM, disk, thermal, process and provider capacity. Queue expensive
work with fair resource reservations and bounded waits. Until all launch paths
share reliable capacity accounting, explicitly coordinate heavy builds and gates
on each host rather than run competing full builds.

### Recursive adapter: driver and engine are composable roles

The CEO's recursion adapter vision applies to **every Susi container component**,
not only the harness: a driver can be an engine, an engine can be a driver, and
these roles can nest and recurse across many levels. A component may consume
capabilities from children while exposing its own capabilities to a parent.
Roles are contextual contracts, not fixed vendor identities or a hardcoded
hierarchy. A container component here is an architectural composition boundary;
it does not introduce a Docker or other foreign-runtime prerequisite.

Use explicit capability discovery and versioned contracts for inputs, outputs,
state, lifecycle, cancellation, errors and evidence. Admit a driver only when its
required capabilities are actually available and verified. Nested work retains
traceable parent/child identities and provenance. Delegate only permissions and
budgets already authorized; a child, wrapper or recursive call cannot enlarge
its authority. Enforce cycle detection, termination/cancellation and resource,
time and depth budgets appropriate to the workload. Unbounded recursion,
self-replicating work and repeated side effects are not flexibility. Preserve
isolation and idempotent retries, and propagate failure rather than manufacture
success. These are implementation requirements, not claims that Susi already
matches every other agent or that recursive adapters are deployed.

## 3. Fast main: unrestricted development publication, gated releases

Agents may commit, push and merge Susi work without routine CEO approval. The
CEO explicitly directs **no mandatory gates for main branch pushes**. Main is an
**unqualified development integration stream**, not a production or release
attestation. Agents may contribute as often as work is ready; no fixed push
cooldown, artificial agent quota or mandatory pre-push test queue applies.
Focused development checks remain useful and their results must be reported
honestly, but they do not become a hidden main-push prerequisite.

Susi's master brain coordinates distillation, branch composition and serialized
integration/publication. Preserve every accepted contribution, reconcile the
current target head, and use an atomic normal fast-forward update. If the target
moves, reconcile against it and retry without overwriting history. No force-push,
blind ref reset, destructive overwrite or touching another worker's claim.
A clean merge does not qualify a release, and main may contain unfinished work;
keep its status and known failures visible to other agents.

**Release branches and release snapshots require the full exact-tree gates.**
Susi selects and distills development contributions into an identified release
candidate, then verifies that exact combined tree with the tests, audits,
provenance and artifact evidence below. Never move an unqualified main artifact
over installed production or imply that a successful push is a release.

Existing `cargo local-workflow ship` and `IntegrationPolicy` may still encode
mandatory main gates or branch-only publication. This is implementation drift
to migrate, not the CEO's current policy. A normal authorized Git publication
that preserves history may carry the development change while that tooling is
updated. Repository access controls remain binding: do not alter branch
protection, rulesets, identities, authentication or credentials, and do not
route around a denied action. If live controls prevent publication, report the
exact blocker and retain the isolated contribution for later integration.
## 4. Portable Git storage, native coordination

Git is the portable shared storage and history format. GitHub, when used, is
storage only. No GitHub Actions, hosted workflow dependency, pull-request/status
API prerequisite, vendor lock-in or required third-party coordinator. The only
remote `refs/heads/*` branch on GitHub is `refs/heads/main` (CEO directives
2026-11-11): every other `refs/heads/*` branch (`local-agent/*`, `release/*`,
`release-staging/*`, `distill/*`, `wip/*`) is offered by GitHub as a pull
request that could bypass Susi's integration rules, and is strictly forbidden.
Agents publish their work to the non-branch ref `refs/agents/<agent>`
(`docs/agent-refs.md`), which offers no pull request. Release candidates and distillation outputs
use non-branch refs (`refs/candidates/release/<name>` and
`refs/candidates/distill/<name>`) plus immutable tags
(`refs/tags/susi-distill/<name>`, `refs/tags/susi-snapshot/<name>`, and
qualified `refs/tags/v<version>`). No agent opens, merges, or closes a pull
request, or deletes remote branches without explicit CEO direction. Native
Windows x86_64 and Linux x86_64 development, integration and qualification must
work locally. Keep portable source; do not add unapproved platform release legs.

Susi owns claims, task and feature manifests, evidence, integration and release
records. Use versioned, auditable records and expected-head checks over the
approved Git transport. Local locks alone do not coordinate independent hosts.
Missing distributed coordination remains explicit work. Agents may continue
independent scoped development while a publication queue is busy or offline.

## 5. Distillation preserves intent and capability

The master brain identifies reusable capabilities, resolves duplicate work and
selects proven improvements. Distillation is evidence-backed selection and
composition, never deletion disguised as migration or a model's claim of success.

Every contribution needs a task manifest recording its unique identity, goal,
base/source commits, exact scopes, dependencies, affected capabilities, acceptance
commands and actual results, ownership, status and remaining limitations. Feature
manifests record feature identity, source inputs, dependencies, enabled/disabled
capabilities, destination branch or product, license/provenance and validation.
Record why an item is retained, combined, deferred or rejected. Existing
`.agents/tasks/` records remain the task format; these additional feature and
distillation requirements must be implemented and verified, not assumed present.

When moving a capability out of core, first land its working destination on the
authorized commercial origin with real module wiring, compilation and tests.
Only then remove the source implementation in a separate change citing that
commit. Preserve the capability through the protocol socket, or remove an
explicitly retired port together with its callers and tests. No error stubs or
silent capability loss. Preserve source authorship, licenses and attribution.

## 6. Pristine branches and immutable release snapshots

Development `main` is the fast, unqualified integration stream. Susi derives feature,
distillation and release-candidate branches from explicit manifests and exact
source commits. Branches are **versioned and may advance through changes**; release branches
advance only with the required qualification evidence. A release tag is an **immutable snapshot**: never move, overwrite or
reuse a published tag. A corrected release gets a new tag and release record.
Do not describe an advancing branch as an immutable tagged snapshot.

Every release binds the tag to the exact commit/tree, manifest, locked dependency
graph, compiler/toolchain, target/features, license and Rust-only audits, nonempty
test results, executable smoke evidence and artifact hashes. Keep tagged source
and its records reproducible. Build isolated debug and production candidates;
promote production only after qualification, with atomic installation, health
checks and a tested rollback to the prior verified artifact. A branch name, tag
or successful push does not certify production readiness.

Branches do not create confidentiality boundaries. Preserve each repository's
actual access boundary: private commercial source, private product plans and
credentials never enter public core branches, tags, manifests, logs or artifacts.
A private branch in a public repository is not a private repository. Do not widen
access or move content between access domains without explicit authorization.

## 7. Lean core and free Apache-2.0 foundation

Core remains the lean standard-protocol socket built around **GAWD, GEMI, GMCP
and the harness**, Apache-2.0 and free forever. Other product capabilities are
independently admitted plugs; proprietary commercial implementations stay in
`susi-commercial-apps`. Do not embed vendor products into core or shift commercial
code into the public repository. Ratchet measured source, dependency, executable,
memory, latency and build costs down without losing required capability.

Rust-only implementation and native Windows requirements remain. Distributed
components retain the existing Apache-2.0, MIT, BSD-2-Clause and BSD-3-Clause
allowlist with correct SPDX interpretation, nested notices and linked standard
library provenance. A facade, protocol connection or passing top-level audit
does not relicense foreign code or prove the shipped artifact compliant.

## 8. Fast evidence, honest autonomy

Use focused checks and compatible immutable caches for development feedback.
Reuse evidence only when its source, dependency closure, environment, toolchain,
features and required checks are still valid. The release gate verifies the exact
combined tree. For release qualification, unknown impact fails closed to the wider
check; zero executed tests is not a pass. Main publication is not release
qualification and has no mandatory pre-push gate.
Measure cold and warm latency, queue time, build, test, startup, useful response
and end-to-end delivery separately. Report counts, inputs and failures honestly.

Autonomous mode is engineering authority within the user's boundaries, not an
approval bypass. Existing privacy, security, credentials, spending, destructive
action and external publication limits remain. Secrets stay in approved secure
credential handling; no blanket credential transmission is authorized. The
master brain cannot self-approve missing permissions or certify its own success.

## 9. Every agent's start and finish checklist

1. Read `AGENTS.md`, this mandate, current task state and any reachable authorized
   coordination board. Record the mandate revision used; do not follow a stale
   copied brief over a newer CEO direction.
2. Inspect actual host, model/tool availability, repository head and existing
   work. Reserve one unique, non-overlapping native claim in an isolated worktree.
3. Maintain the task/feature records, implement, review and run meaningful checks
   in private scratch state. Keep independent work moving without heavy-build
   contention. Report exact blockers instead of inventing capabilities.
4. Integrate development work without mandatory main-push gates. Preserve other
   contributions, reconcile the current head and use a normal fast-forward push.
   Verify the target origin contains the commit before reporting it landed.
   Report main as unqualified, and local-only or pending work as such.
5. Distill and release only with the manifest, provenance and immutable-tag
   requirements above. Preserve working production and rollback. Close the claim
   only after acceptance; leave an actionable handoff if unfinished, including
   at 10% weekly allowance remaining.

## Supersession and implementation truth

This 2026-11-10 direction supersedes the fixed remote push cadence and the
single-CEO-laptop-only integration role in the 2026-11-08 remote mandate, and
mandatory main-push gates in earlier workflow language. Full exact-tree gates
now apply to release branches and release snapshots. Its
incident safeguards remain: owned identity, scoped changes, release exact-tree tests,
destination-first migration, no stubs, preserved history and honest reporting.
Existing branch-only `IntegrationPolicy` checks and other older implementation
restrictions may still be present. Agents must update them through a scoped,
reviewed change with honest validation, while obeying live access controls. This document
itself launches no agents, changes no security settings, deploys no runtime and
certifies no feature as complete.
