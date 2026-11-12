# Susi agent instructions

Read `AGENTS.md` before editing. It is the canonical engineering contract and
contains the CEO's agent-only, Rust-only, native Windows and local-development
requirements (identity.json Mandates 62–79). Follow its native isolated workflow
and evidence gates; do not substitute legacy shell or GitHub prerequisites.

The CEO never inspects code. Agents own implementation, verification, integration
and production qualification. Requirements are not claims of current compliance.

The CEO authorizes agents to commit, push and merge without routine CEO approval.
Follow AGENTS.md serialized integration and history-preserving publication;
this authority does not waive production gates or authorize unrelated spending,
destructive actions or external publication.

Read docs/ceo-agent-mandate.md (Mandate 73): an available authorized driver (dot, duet,
another agent or capable Susi harness) coordinates each laptop. Cloud sessions
are independently autonomous within user authorization. Driver/engine recursion
applies to every container component through capability contracts and budgets;
nested work cannot amplify permissions or bypass cycle/termination checks.
Susi owns distillation and serialized integration. Main is unqualified development
with no mandatory pre-push gates; full exact-tree gates apply to release branches
and snapshots, with normal fast-forward updates and no shared-history overwrite. No arbitrary
agent-count or push-rate limit; measured hardware capacity and evidence govern.
Published release tags are immutable. Preserve confidentiality and access controls.

Read docs/ceo-mandate-74.md (Mandate 74, wins where older text differs): work in
your own isolated worktree with remote pull and merge reconciliation at every
workflow step (`start`, `sync`, `commit`, `distill`, `ship`, `release`, and
parallel harness steps), keep Susi core build+test+release under 10 seconds
(`<10s` warm pipeline budget) across its 5 free-forever Apache-2.0 pillars
(`GAWD`, `GEMI`, `GMCP`, `ACP`, and `Susi Harness` with parallel multi-agent
workflow), dump labelled `Task:` work straight into main with no gates and no
PRs, let Susi deterministically distill qualified releases with conflict-free
parallel picks, harvest pure Apache-2.0 code with full attribution, keep all add-on
features, UIs, SSO/RBAC, FinOps and enterprise packs closed, licensed and paid
for corporates in `susi-commercial-apps`, and treat GitHub as a storage bucket only.

Every session, before anything else (CEO directives 2026-11-12): run
`susi-commercial workflow check` in your own worktree. It pulls the latest
remote into a clean worktree (Mandate 74: always work on the latest remote,
before starting, when resuming and before every commit), shows the Susi Board
(Mandate 76: always work in collaborative mode, one agent or many, and never
assume you are alone), and keeps the installed release updated (Mandate 48:
keep the local release binary at the latest release qualified for this host).
Before writing new code, look for what already exists, inside Susi and among
Apache-2.0 code, libraries, frameworks, tools, products and services, and
record where you looked in the task record (Mandate 78).

Read docs/ceo-mandate-79.md (Mandate 79, 2026-11-12): Susi keeps improving like
AI models do. Apache-2.0 projects are the raw source; both repositories are the
branches they are distilled into. Record any vision the CEO or a delegate sets
(`susi-commercial workflow vision set`), run `susi-commercial workflow vision
cycle` after `workflow check`, take up untaken visions and look again at every
harvested upstream that has moved.

Read docs/ceo-mandate-78.md (Mandate 78, 2026-11-12): never reinvent the wheel.
Before building a capability, look for it inside Susi, then in Apache-2.0
projects, then in the locked dependency graph; reuse by harvesting only the
needed part under Mandate 75's rules; delete the hand-rolled code a reuse
replaces; stay inside the lean ceilings and the `<10s` budget; record the
decision in the task record's `reuse` object.

Read docs/ceo-mandate-77.md (Mandate 77, 2026-11-12): work only under your own
agent root, one directory per agent per machine, marked with `.susi-agent-root`.
Find it with `susi-commercial workflow root show` or register it with
`susi-commercial workflow root init --agent <name>`; keep every clone, worktree,
build target and scratch file inside it; never create, change or delete anything
under another agent's root or the CEO's working directory.

Read docs/ceo-mandate-76.md (Mandate 76, 2026-11-12): collaborate with every
other agent through the Susi Board. Read it at session start and before you
claim, publish or cut a release; post a Claim before the first edit and Landed
after the commit is on `origin`; append only and never rewrite another agent's
item; ask the owner on the board instead of touching a claimed scope; answer
what is addressed to you; keep it free of secrets, commercial source and any
hosted-service dependency.

Read docs/ceo-mandate-75.md (Mandate 75, 2026-11-12, wins where older text
differs): operate both `susi` and `susi-commercial-apps` strictly on the Raw
Source (`main`) -> Distill (`release`) model (`refs/heads/main` ungated raw
feature stream with final-paragraph `Task:`/`Feature:`/`Harvest:`/`Origin:`/`Origin-License: Apache-2.0`
trailers -> Susi-owned `refs/candidates/release/*` distillation with
one-version-one-cut `release_version_guard` -> immutable `vX.Y.Z` snapshots),
freely pick up capabilities from cloud raw sources (`local-harvest raw-features`,
`route`, `pickup`) into Susi Core's 5 `<10s` lean socket pillars or into closed,
licensed, paid `susi-commercial-apps`, enforce **Pure `Apache-2.0` ONLY** for all
cloud raw-source harvesting (full Apache-2.0 section 4(a)–(d) notices, refusing
standalone MIT/BSD, `Apache-2.0 WITH LLVM-exception`, `AND`, copyleft and
unlicensed raw source), and coordinate additively across `Claude`, `Codex` and
`Carbon` over portable Git storage and the shared Susi Board.
