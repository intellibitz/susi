**START HERE: read this active contract, inspect local state, and reserve an isolated worktree before editing.**

# Active engineering contract

Susi core is a generic standards-protocol AI OS socket. The requirements below
are not claims of completed implementation. Follow the current user instruction
where it supersedes older repository text. Agents own authorized development,
verification, integration and production qualification end-to-end; do not ask
the requester to inspect code or perform routine engineering steps.

## Core boundary and size

- Preserve exactly three core pillars: **GAWD** (A2A, ACP and the parallel
  harness), **GEMI**, and **GMCP**. Preserve their existing public capabilities,
  wire contracts, errors, authorization and compatibility while reducing core.
- Core contains generic protocol contracts and socket machinery. Concrete
  implementations belong in separately hosted implementation packages, including
  `susi-commercial-apps`. Prove the destination works through the standard
  socket before removing its implementation from core. No functionality is
  dropped, no error stub replaces a working capability, and no implementation
  is merely relabeled to reduce the count. A capability moved into
  `susi-commercial-apps`, working there with old and new behaviour shown to
  match, is not dropped (a recorded decision): core forwards to it, and an
  installation without it names the command that serves it. Keep proprietary
  source out of core.
- The complete socket must be under 10,000 physical runtime Rust lines,
  including blanks, comments, inline tests, every feature/platform and the
  `susi-vendor-*` facades. Until that goal is met, core never grows. Pay for any
  runtime addition in the same change and lower `COMPLETE_SOCKET_CEILING` in
  `tests/lean_core_ceilings.rs` after every reduction. Never hide or minify code,
  move it into an excluded category, or weaken acceptance to manufacture a pass.
  `cargo core-size` is the explicit goal check; its passing is not protocol
  certification. Use the counting contract in `docs/lean-core.md` unchanged.
- Keep runtime and current documentation generic: no personal, corporate or
  individual identity references. Preserve protocol identifiers and mandatory
  license/attribution notices. Provider-specific compatibility moves with the
  implementation; blind renaming must not break clients or refusal boundaries.
- Core ships no UI or bundled plug content. Existing product implementation
  crates are migration debt and may only shrink. Keep the dependency, unsafe,
  license and source-size ratchets; do not raise one without explicit authority.
  The unsafe ratchet's tools are built from a Susi Commercial Apps release and
  run against core's reviewed baselines (`.agents/baseline/`).
- Zero required user configuration means discovery, defaults and agent repair.
  Measure size, memory, latency and build cost. The warm build + test + release
  budget is `<10s`; the cold compile target is `<60s`. Report cold and warm
  separately. The focused warm pipeline never substitutes for full qualification.

## Ownership, collaboration and delivery

1. Use your own registered agent root, isolated worktree, branch and identity.
   Keep source, build targets, runtime homes, scratch files and ports private.
   Never edit the primary checkout, another agent's files or the requester's
   working directory. Roots never nest or overlap; register an existing root in
   place. Reuse your own idle worktrees where appropriate.
2. Inspect current changes and active claims before editing. Reserve disjoint
   scopes using available native `local-workflow` tooling (core's development
   and release tools are built from Susi Commercial Apps' `tools/core-xtask`
   and run from a core checkout). Keep one live
   claim per worker and renew active progress. Do not hold a claim for work you
   have stopped, handed off or completed; release it in the same step. The lease
   is a last resort, not the release. Preserve unpublished work or deliberately
   abandon it with explicit authority; never silently discard it.
3. Collaborate through the authorized coordination surface, respecting other
   owners. Read relevant state before claiming, integrating or publishing;
   resolve a stop instruction within its scope. Do independent work while a
   dependency is pending. Send external messages only with explicit user or
   invoked-workflow authorization. Coordination text is not acceptance evidence.
4. Fetch and merge `origin/main` before starting, when resuming and before each
   commit if reachable. Resolve conflicts while preserving accepted work; do
   not rebase or overwrite shared history. Offline local development remains
   available. A failed remote is reported, not mistaken for synchronization.
5. Commit, push, merge and pull about every ten minutes, meaningfully. This is
   a cadence against drift, not a hard limit: no empty commits or noise, no
   refusal merely because time elapsed. Use serialized integration and normal
   atomic fast-forward publication. Never force-push or change live access
   controls. Verify the remote contains the commit before claiming it landed.
6. Agents may commit, push and merge authorized work without routine approval.
   This scope does not authorize unrelated spending, destructive operations or
   external publication. Main has no mandatory pre-push gates and remains
   unqualified development; full exact-tree gates apply to release branches and
   snapshots. Live controls are not bypassed because tooling lags the contract.
7. Core is Apache-2.0 and published one way. The designated publisher alone
   holds push authority. Authorized agents may exercise that authority within
   their scope; this policy grants no additional write access. Accept no outside
   pull request, patch or commit. Everyone is free to fork under the license;
   the license grants code rights, not rights to the Susi name or marks.
8. Use portable Git storage, without hosting-service APIs, hosted CI, hosted
   pull requests or required remote review. Only `refs/heads/main` is published
   as a branch. Agent handoffs use `refs/agents/<agent>`; qualified release lines
   use fast-forward-only `refs/candidates/release/<name>`. Release tags are
   immutable. Preserve the source, artifact and license binding of each release.
9. Use `xtask local-build debug` for a native development candidate. Guarded
   commits carry task ownership (`Task: T-<AGENT>-<n>` or native
   `Task: LOCAL-<name>`). Keep active task scopes and meaningful acceptance
   available to native tooling; a reservation or successful compile is not
   acceptance. Close only after the entire acceptance check passes on the actual
   combined tree. A partial result never closes a broader goal. Keep task
   records tidy: an active record's place is its state, so no status in it says
   done, failed or canceled while it is open. Close a finished task through its
   acceptance; retire one that will not be done with its reason instead of
   marking it; add no duplicate; keep every record readable, with its goal,
   size and acceptance; and review an open task nobody has claimed or touched
   for 14 days.
10. Clean up finished worktrees only after every commit is on `origin/main`,
    the tree is clean, no live claim or running agent owns it, and no next task
    is reusing it. Remove only your own worktree, merged branch and private
    build target. Never remove the primary checkout, the requester's checkout,
    another agent's work, unpublished commits or shared build caches. Check
    ownership and state again immediately before removal.

When shared coordination messaging is authorized, keep it tidy by appending
closures rather than editing or deleting items. Close what you open, answer
addressed requests and post facts once without noise. Only the originating owner
or an authorized decision may lift a stop. A request with no reply for two days
may be closed if both parties have also been silent for two days; a claim may be
closed two days after its lease ends. Before leaving, reconcile those entries,
release claims and clean eligible worktrees within the same authorization scope.

## Records and verification

Operational history, completed tasks, growing facts, evidence and audit records
belong outside checked-out source, in a local database when available; historical
records may be absent. Do not add source files or append mandate histories merely
to retain them. Minimal active coordination may remain while native tools need
it, and is removed on completion. Required license notices and live recovery or
authorization data retain their own correctness requirements. Missing history
never proves acceptance, a completed dependency or a qualified release.

Use durable local records for outcomes that require later verification. Record
current goals and revisit unfulfilled goals and moved upstream dependencies;
never claim progress from a missing record. Keep goals tidy: record each one
once, in the words it was given, and never edit it; link every task that serves
it and never strip the link when rewriting the task; when a goal lives on as a
standing rule, retain its location in a create-only carried note in local storage,
which never claims the goal met; account for every goal, taking up or carrying one whose
tasks are all closed. Preserve required artifact provenance
with the artifact, independently of optional operational history.

Keep the contract tidy. Review the current contract in place instead of adding a
new numbered historical mandate. State each rule once, here or in the one
document this contract links, and point to it elsewhere instead of copying it.
When a rule changes, correct every older text it touches in the same change, so
no two current texts disagree. Keep every link and named command resolving, and
every implementation-truth note true, updated in the change that alters what
holds its rule. Agent rule files stay identical pointers that restate nothing.

Keep documentation tidy. A document says what is true now and changes in the
change that alters what it describes; correct or remove it when its subject
goes. Keep one topic in one document, found through the documentation index
(`docs/README.md`) and extended rather than copied, and list every new document
there. Keep every relative link and named command resolving. Dated records say
their date and are listed as records; quoted upstream text keeps its words and
links.

The local delivery goal remains a verified live executable in under 60 seconds,
with declared millisecond response budgets. Measure cold/warm build, test,
startup, goal acknowledgment, first useful output and completion separately;
include repetitions, failures and rollback. Measure from change ready through
integration, qualified promotion and observed changed behavior. Demonstrate
plugin updates over the socket without rebuilding core. Report removed source,
dependency reach, artifact size and wall-clock cost on identified trees.

Available drivers coordinate local or cloud agents through capability contracts.
Driver and engine roles compose across every container component with cycle,
termination/cancellation and resource/time/depth budgets. Nested work cannot
amplify permissions. Concurrency follows measured capacity, not a fixed agent
count; autonomy does not grant credentials, spending or destructive authority.
Default-driver status requires measured parity. Distill labelled work into
conflict-free release candidates with exact-tree qualification and atomic
publication; retry a lost integration race against the new head.

## Native implementation and licensing

- Runtime, build/install/release tooling and shipped implementation dependencies
  must be Rust. A facade does not turn foreign runtime code into Rust. Installed
  compiler/linker tools and OS ABI calls are explicit platform boundaries.
- Core ships for x86_64 Linux only (decision of 2026-11-18, Mandate 84);
  qualify releases only on a native x86_64 Linux host. Windows is served by
  Susi Commercial Apps, which installs and manages the Windows channel; the
  Windows code still in core is migration debt that only shrinks. Development
  on a Windows host uses native MSVC and OS APIs with no Bash/WSL/MSYS, Python,
  Node or container prerequisite. Keep portable source for other targets
  without adding their builds, tests or release legs. Optional legacy scripts remain migration
  debt, never prerequisites for native users.
- Before building, inspect both repositories, suitable Apache-2.0 work, and the
  locked dependency graph. Prefer reuse of the needed part; delete what it
  replaces. Keep the decision in active/local task state. A new dependency needs
  necessity and qualification; core growth and capability loss remain prohibited.
- Copied/harvested source must offer pure Apache-2.0, with that alternative
  explicitly elected for dual licensing. Preserve section 4 notices, licenses,
  modification attribution and pinned source provenance. Reject unlicensed or
  incompatible copied source. Never relabel third-party code.
- Distributed dependencies permit Apache-2.0, MIT, BSD-2-Clause and BSD-3-Clause
  under the existing deny policy. Evaluate AND/OR obligations and nested source
  exceptions correctly. Audit transitive dependencies, native objects, model
  assets, build helpers and statically linked standard-library implementation.
  A Cargo SPDX pass alone does not qualify those sources or generated tables.
- Custom standard-library replacements require source, builder, compiler,
  selected dependency graph and linked-artifact evidence. Do not hide source
  exceptions, broaden the allowlist or omit shipped objects from the audit.
  External services are protocol peers, not proof of local license compliance.

## Engineering rules

- Preserve `forbid(unsafe_code)` and `deny(unsafe_code)` boundaries. Existing
  platform FFI exceptions remain narrowly scoped per function or metadata item
  with `// SAFETY:` reasoning, ownership and lifecycle tests. No module-wide
  allowance, shell escape, process-global handle change or raw-handle leakage.
- Production code has no panic-path macros. Justify every lint allowance inline;
  tests may use the existing test-only allowances. Keep exhaustive enum matches,
  `unsafe_op_in_unsafe_fn` denial and formatting checks.
- Keep code tidy. Before publishing, run rustfmt and Clippy with `-D warnings` on
  every crate a change affects, and leave no `dbg!`. Add no debt: a new lint
  allowance gives its reason, a marker comment names its task or the work is
  done now, and commented-out code is deleted. A file over 2,000 lines grows by
  at most 25 lines in a change; new code goes into a focused module. Remove what
  nothing uses in the change that leaves it unused. Follow the crate's existing
  patterns; comments say why. Clear debt you meet in its own commit.
- Keep commits tidy. Each commit is one whole change that builds and passes what
  it affects, with its task and other trailers together in the final paragraph.
  Its subject says what in at most 72 characters, with no trailing period; a
  change over 100 lines has a body that says why, what it found and what it
  leaves out. Commit only source: no build output, scratch, local state, blob of
  1 MiB or more, or secret; rotate a committed secret at once. Never rewrite
  published history; correct a published mistake with a new commit.
- Return typed `Result<T, EaiError>` from I/O, state and IPC. Prefer infallible
  constructors; preserve poison recovery and pre-epoch timestamp fallback.
- Keep the leaf dependency order `paths → error → config → core/sandbox →
  services → daemon`. No inter-crate edge without necessity. External crates
  remain owned by their `susi-vendor-*` facade; ordinary `susi-*` crates declare
  no external normal/build/dev/target dependency. Avoid duplicate declarations.
- Check HTTP status explicitly: transport success may include non-2xx responses.
  Preserve body bounds and overflow rejection. Standalone services enable only
  their own feature; root service dispatch may enable all services.
- No stubs, fake success, dead public interfaces or lowered acceptance baselines.
  Inspect every platform/feature configuration before migration. On the exact
  migration tree run `cargo check --workspace --all-targets` and formatting.

## Secrets, testing and releases

Use only the authorized automation vault or scoped process environment. Never
open a private personal vault, invent a secret, print a value or place it in
source, logs, chat, task records or command-line arguments. Portable encrypted
vaults retain cross-platform unlock; local unattended unlock stays bound to the
host's native mechanism. Inject only a child's required scope, through pipes or
its environment. Preserve refusing protected vault paths and all compatibility
checks until the implementation is safely migrated. Never rotate an authority
secret or alter credential access as routine cleanup.

Tests are hermetic: private HOME/USERPROFILE/XDG/SUSI_HOME, scratch configuration,
ports and local Git remotes; no production state or inherited credentials. Clear
child environments and prove fixture routing before side-effecting test actions.
A command named `check` may mutate: inspect its behavior rather than trusting its
name. Do not send real coordination messages from tests.

Keep tests tidy. Before publishing, run the tests a change can break: its crates,
their dependents, and the root tests that read changed documents or data. Fix a
test your change turned red at once; when its author has not fixed it within an
hour of a coordination request, any agent may. Never skip, ignore, loosen or
delete a test to get green. Each test proves one named behavior; an ignored test
gives its reason and an expected panic names its message. Wait on conditions
rather than fixed sleeps. Pin a count or phrase only when it is the contract, and
move the pin in the change that moves it. Remove tests and fixtures with the code
they cover.

Use the pinned compiler and committed lockfile. Scope development checks to
changed crates and dependents, and run all required checks before qualification:

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets --offline --locked -- -D warnings
cargo test --workspace --offline --locked
cargo deny --offline --locked check licenses
```

Do not enable mutually exclusive accelerator features together. Offline Cargo
does not sandbox build-script network activity; verify sources and build scripts.
Nonempty executed tests are required; ignored tests and compilation are not passes.
Report limitations and keep unmet requirements open.

Keep debug and optimized candidates separate from installed production. Production
promotion requires exact source/lock/toolchain/target/feature evidence, license
and Rust audits, required tests, executable smoke tests and an artifact digest.
Use atomic replacement, health checks and tested rollback to the previous verified
artifact, including interruption and native file locks. Never install an
unqualified candidate as production. A release qualified for this host is complete
only when the installed binary, `susi.build.json` and live daemon are synchronized
and healthy. Reuse the qualified artifact after digest verification; keep
scheduled Windows tasks at `RunLevel=Limited`. Keep the installed release current
when a newer release is already qualified for this host, its remote is reachable
and no client is mid-mission; verify its artifact and retain rollback. Source
publication, a model claim
or an immutable tag alone does not establish production qualification.

Keep releases tidy. Cut each version once and never move, re-cut or delete its
tag; follow a broken release with a new version. Publish the tag, its record and
its qualification record together, agreeing on the commit. Each release says what
it ships, from its commits' task trailers. Open a candidate line only when none
serves the version; never move one, and retire it once it has ended (below).
Remove local build outputs and test-release directories once the release is
published.

Keep candidates tidy. A release candidate is `refs/candidates/release/vX.Y.Z-rcN`
and a distilled feature `refs/candidates/distill/<feature>`; one version has one
open line, advanced by fast-forward, with a new rcN only when the open line
cannot fast-forward. Open a line under a task and announce it; release or settle
a line that has not moved for three days. This contract authorizes deleting a
candidate line once it has ended (released, superseded or merged into a release):
delete exactly those lines, each only at the object observed, in one atomic push,
and record each in `.agents/storage/directed-deletions.txt` in the same session.
Never delete an open line or any other ref under this authorization.
