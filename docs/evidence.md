# Evidence: verification and qualification

Current documentation describes contracts, reproducible checks and known
limitations. A past result applies only to the source, dependencies, toolchain,
features, platform and artifact that were checked. It does not qualify a later
tree or establish a capability that has no working execution path.

## Record storage

Keep investigation logs, task histories, measurements and other run records in
an agent-owned local store outside the checkout. A database may hold retained
records; disposable history need not be retained. Do not grow source files,
Markdown pages or `.agents/` with append-only activity or evidence logs.

This storage direction does not claim that every existing tool has a native
database backend. Some tools still write file receipts or read source records.
Use an external output directory where supported; migrating the remaining
writers and functional record readers is implementation work. Preserve required
authorization and recovery behavior until a working replacement is verified.

GMCP's native `local-release record-gmcp` stores bounded current snapshots
under ignored `.susi/records/gmcp/evidence/`, with legacy source reads. See
[GMCP evidence](../.agents/gmcp/evidence/README.md) for both recording phases and
the exact-source and mastery-input bindings. No history is proof by itself.

Qualification evidence must remain available with the artifact it qualifies,
outside the source checkout. Keep license, copyright and required attribution
notices with distributed source and artifacts. Optional run history does not
replace those obligations.

## Verified claims

For a measured result, record the exact command, source revision and tree,
dependency lock, compiler, target, selected features, relevant configuration,
artifact digest and observed outcome. Identify which tests actually executed;
an empty selection, an ignored test, or a successful compile is not a passing
behavioral test. Report failures, skips and untested platforms explicitly.

Size measurements retain the complete counting boundary. `cargo core-size`
checks the under-100,000-line goal; a successful size result alone does not
prove preserved functionality or standards conformance. See
[Lean core](lean-core.md) and [Host contract conformance](host-contract-conformance.md).

## Platforms

Core release qualification and publication cover x86_64 Windows with native
MSVC and x86_64 Linux. Portable source for other targets remains source support,
not evidence that a release was built or tested there. A Linux run does not
qualify Windows, and vice versa. Hosted CI is not a prerequisite.

## Building susi: the workflow

Agents own development, verification, integration and production qualification.
Follow [AGENTS.md](../AGENTS.md) and the
[native bootstrap](native-bootstrap.md). Use an isolated worktree, private build
output, private runtime home and owned ports; never test against production or
share mutable development state between workers.

Run checks appropriate to the changed capability and its callers. Verify the
actual combined tree after integration. A clean merge or an earlier component
result is insufficient when the integrated behavior has changed. Commands,
outputs and measurements belong in the external local record store described
above, rather than a new source ledger entry.

Main is unqualified development source and has no mandatory publication test
gate. Publishing source does not qualify or promote a production artifact.
See [Native delivery](native-rust-direction.md) and
[isolated cloud sessions](cloud-sessions.md) for the delivery contracts.

## Production qualification

Bind qualification to the exact source, dependency and toolchain identity,
passing tests and policy audits, executable smoke checks and artifact digests.
Keep debug builds and optimized candidates separate from the installed release.
Promotion requires an atomic replacement, runtime health verification, a retained
known-good artifact and a tested rollback path. A snapshot or a locally built
candidate is not an approved release.

Release checks include native platform behavior, dependency and linked-source
license compliance, required protocol behavior, credential and permission
boundaries, resource limits and the installed-product path. An offline Cargo
resolution or warm cache does not establish clean-machine independence.

<a id="admitted-gaps-on-record-2026-11-08"></a>

## Limitations and open work

Keep current gaps explicit in the capability's contract and reproducible checks.
Do not turn old measurements into current guarantees, silently remove a failing
case, or present a missing implementation as success. A capability moves out of
core only after its destination works and old/new behavior is demonstrated over
the socket, including relevant platform-specific callers.

The generic-core boundary, off-source record storage and remaining implementation
migrations are requirements, not statements that all existing code already
conforms. Consult the executable source and run the relevant checks before
making a current capability or production claim.

- The source-size goal and implementation extraction remain incomplete. See
  [Lean core](lean-core.md) for counting and acceptance requirements.
- Passing sampled host checks is not complete standards certification. The
  [conformance contract](host-contract-conformance.md) names coverage and gaps.
- A development build is not installed-product qualification. See
  [Step 1 acceptance](step1-acceptance.md) and
  [native delivery](native-rust-direction.md) for the required gates.
- A missing implementation must report its capability gap. The
  [architecture](architecture.md) describes the existing plug boundaries;
  a documented target does not establish a working destination.
