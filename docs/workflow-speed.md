# Workflow speed: where the minutes go, and what moves them

Measured on 2026-10-30 (Windows 11, 28 logical cores, the shared cargo
target warm; GitHub-hosted runners). The numbers are the reason for the
settings this page names; re-measure before changing them.

## The loop, timed

| Step | Measured | Where the time is |
| --- | --- | --- |
| `scripts/susi-worktree.sh` | 24 s | checkout of 3 800 files |
| `susi workflow check` | 13 s | remote reads (`gh`, `git ls-remote`) |
| local gate, root package change: clippy | 2 m 19 s | 93 crates recompiled, 11 of them registry crates (see *feature sets*) |
| local gate, root package change: test build | 4 m 08 s | the 100-odd root test binaries |
| local gate, root package change: tests | 15 m 16 s on 8 threads | 598 tests, 7 188 s of test time against 1 177 s for the same tests on a 4-vCPU Linux runner |
| branch push → Test run green | 1–8 min | `Compile Check`: 1 min on an sccache hit, 8 min cold |
| green run → merged | under 1 min | `auto-merge.yml` |
| merge → main suite green | 10–27 min | does not gate the merge; the release waits for it |
| tag → release published (v0.21.26) | 92 min | 19 min waiting for the suite, then the slowest leg (macOS 38 min; Windows 30 min after a 43 min wait for a runner) |

Hours came from three places: the local tests (a root change is 22 minutes of
gate, most of it running tests six times slower than CI), every worktree
sharing one cargo target (one build at a time across all agents, so N agents'
gates serialise), and a release that put the whole suite in front of its
slowest leg.

## What changed

- **Release legs build while the suite runs** (`release.yml`). `tests` proves
  the suite; `build` and `build-cuda` start with it and store workflow
  artifacts; `publish` attaches them once both have passed. Wall time is the
  slower of the suite and the slowest leg, not their sum. Nothing publishes on
  a red suite.
- **The release reuses the in-flight suite** (`scripts/release-await-tests.sh`).
  The merge bot dispatches the post-merge suite on main and then the release
  on the tag, so the suite is nearly always running when the release starts.
  It used to dispatch a second one (two 19-minute suites side by side, every
  branch push queued behind both); it now watches the running one and
  dispatches only if that turns out to be a branch gate.
- **Thin-local LTO for the published binaries** (`CARGO_PROFILE_DIST_LTO=false`,
  16 codegen units, set in `release.yml`, `release-warm.yml` and
  `dev-release.yml`). Under `[profile.dist]`'s fat LTO the single `susi` unit
  ran 988 s on one thread after the ~700 dependencies finished; thin LTO ran
  it on every core but still left a cross-crate stage after the last crate
  (8 min 21 s of the v0.21.29 Windows leg, 5 min 40 s of macOS). Thin-local
  optimises each crate across its own units and has no such stage: faster
  releases, without cross-crate inlining (the operator's call).
  `Cargo.toml`'s `[profile.dist]` still says fat: the workflows override it.
- **Hermetic test homes live in the clone's git dir on Windows**
  (`scripts/check-hermetic-tests.sh`, `.git/susi-hermetic/`). The tests write
  thousands of small files (scratch worktrees, copied fixtures, instance
  homes); on Windows the user's temp is scanned on every write by real-time
  protection, while a developer's checkout is usually excluded. One I/O-bound
  test: 180 s in the user's temp, 141 s in the checkout, 16 s on a Linux
  runner.
- **More test threads locally, with reservations** (`.config/nextest.toml`,
  `scripts/check-hermetic-tests.sh`). The gate runs one nextest thread per
  logical core instead of eight; the process-spawning tests (gmcp
  requirements, the workflow and worktree suites) reserve two slots each, so
  at most half the threads run those at once — the CPU headroom their
  deadlines assume. CI keeps eight. Measured on the root package: 916 s → 641 s
  for the same 600 tests. The sum of per-test times rose (7 188 s → 11 442 s),
  which says the tests wait on the disk, not the CPU: the real-time-protection
  exclusion below is what the remaining time is waiting for.

## What the operator must do (one-time, this machine)

Real-time protection excludes `F:\src\github.com\intellibitz\susi` (the
primary checkout) but not the sibling worktrees every agent actually builds
and tests in (`F:\src\github.com\intellibitz\susi-<name>`). `MsMpEng.exe` was
the top CPU consumer on the machine (49 000 CPU-seconds) while the gates ran.
Exclude the parent directory once, as administrator:

```powershell
Add-MpPreference -ExclusionPath 'F:\src\github.com\intellibitz'
```

Also running: `SearchIndexer` (21 800 CPU-seconds) and `Everything`
(20 300) index the same tree. Exclude `F:\src` from Windows Search, and from
Everything's index if it is not needed there.

## A compiler cache across worktrees (2026-10-31)

Each worktree now builds into a target of its own, copied from a snapshot
of origin/main's registry crates (`scripts/seed-target.sh`; the shared
target and its guard are retired), so N agents build at once and nothing one
worktree built stands in for another's. What the snapshot cannot hold still
compiled cold in every new worktree. `scripts/setup-sccache.sh` makes every
cargo on the machine compile through sccache, with the cache beside the
snapshot (`<primary>/../.sccache`, 50 GB): `scripts/susi-worktree.sh` and the
SessionStart hook run it where sccache is installed, `--install` installs
sccache, `--status` shows the hit rate, `--off` removes it.

A brand-new worktree's first gate build for the root package (clippy, then
the test build), same commit, same machine:

| | clippy | test build | total |
| --- | --- | --- | --- |
| cold cache | 2 m 28 s | 6 m 32 s | 9 m 00 s |
| warm cache | 1 m 43 s | 4 m 46 s | 6 m 29 s |

The hits are the registry crates a `-p susi` gate unifies with other
features than the snapshot's whole-workspace build (tokio-stream, tower,
tower-http, axum). The misses that remained, and they were the heavy ones
(49 s of compiler time each on average), were rmcp, ra2a, tokenizers,
fastembed and the three candle crates: every one depends on `reqwest` or
`esaxx-rs`, the two `[patch.crates-io]` crates, which were path crates under
`vendor/`; a path crate's rlib embeds the worktree's path, so their
dependents' cache keys differed in every worktree (two different keys for
rmcp, observed), and the same two path crates kept the chain out of the
snapshot.

## The patches as a pinned git revision (2026-10-31)

The two patches now live on this repository's `nopr/vendor-patches` branch
and the root `Cargo.toml` pins a revision of it (`vendor/README.md`). A git
source is keyed the same in every worktree and on every machine, so the
chain above the patches is built once: it sits in the snapshot once the
watcher refreshes it after the merge, and the compiler cache serves it to
every worktree before then.

Measured on the branch that made the change, before its merge (so the
snapshot still carried the old path patches and the chain still had to be
compiled rather than copied): the first worktree built the chain and wrote
it to the cache; a second worktree of the same commit then had 25 of its 26
compile requests served from the cache (the one miss was the root crate),
reqwest, esaxx-rs, ra2a, rmcp, tokenizers, fastembed and the three candle
crates among the hits, where the same crates had been the 14 misses of
2026-10-31's first measurement. Once the watcher refreshes the snapshot from
a main that carries this change, those crates are copied with it and compile
in no worktree at all; a snapshot behind main still has the cache. Wall time
is not reported for that run: another agent's gate shared the machine while
it ran, and the test build (the ~100 root test binaries, which no cache can
hold) was slower than on an idle machine, not faster.

**A hazard found on the way, and its guard.** sccache's client starts a
server when none is running, and that server keeps the client's
environment. Started by the first cargo of a hermetic test run, whose TMP is
a throwaway directory, it kept writing temp files there after the run
deleted it, and every compile on the machine failed with "Failed to create
temp dir" until it idled out ten minutes later: one agent's whole-workspace
gate failed on another agent's server. `scripts/setup-sccache.sh` now starts
the server itself, from a sane environment, with no idle exit, every time it
runs (enabling, and the silent runs at session start and worktree creation),
so a dead server is replaced before a gate can start a bad one, and
`scripts/check-hermetic-tests.sh` asks for the same
(`scripts/setup-sccache.sh --ensure-server`) from the real environment before
it switches TMP, so a hermetic run that finds no server starts a sane one.
`tests/hermetic_scope.rs` pins the order and the environment the server is
started with.

## The compliance check no longer races `finish` (2026-10-31)

`finish` releases the claim seconds after the push, and the branch run's
Workflow Compliance job reads the claim inside that window. The job's
script meant to fetch the claims once, but set its "fetched" flag inside a
command substitution (a subshell), so every read fetched and pruned again:
a release landing between a task's first read (live) and its second (the
scope check) emptied the second, and the commit was refused as work under a
claim that reserves no paths. Three of this session's four pushes passed
only by timing; the fourth was red and green on a rerun, an hour of merge
latency for nothing. The script now fetches once, from the top level, and
judges each task from one snapshot of its claim (`tests/workflow_compliance.rs`
pins it with a git shim that lands the release right after the first
fetch). The same fix removes a remote round-trip per claim read from every
pre-push hook and compliance job.

## Lightweight branch gates skip the workspace target archive (2026-10-31)

PR #1098's branch run took 48 s in `Compile Check`: the affected-package
clippy took 9 s, while restoring Cargo's workspace target archive took 14 s.
Its `Unsafe Exposure Ratchet` job took 26 s, including a 12 s Cargo target
restore for a 5 s ratchet run. PR #1099 added the cache exception for the small,
standalone `susi-unsafe-ratchet` package, but its own compile job took 73 s:
the selector included the root application and its dependents, so target
restore took 24 s, ONNX prefetch 5 s, and clippy 20 s. The crate's test-only
change had been accompanied by workflow, script, and documentation edits; the
selector mistakenly treated those inert paths as production changes and grew
the reverse-dependent closure. This follow-up keeps inert paths out of that
decision. The cache exception should then apply to test-only changes to this
crate; remeasure the next matching branch run before claiming the saved time.

PR #1101's corrected compile job passed in 51 s. Its `Resolve affected crates`
step still took 11 s because the affected-package and ONNX reverse-dependency
helpers each launched `cargo metadata` for the same graph. The gate now writes
one metadata file and passes its path to both helpers; direct script use still
falls back to Cargo. Measure the next branch run to confirm the saved time.

## Push compliance fetches task refs with branch refs (2026-10-31)

The local push guard fetched branch tips, then workflow compliance separately
looked up and fetched live claims, fetched close receipts, and fetched the
branch refs again. On this host a recent `git push` spent more than a minute in
the hook path while many worktrees were active; the logs did not separate Git
network time from host contention. `fetch-workflow-refs.sh` now fetches branch,
claim, and close refs together, and both the local and CI compliance checks
reuse that snapshot. Standalone compliance checks retain their own fetch path.
The fixture verifies one fetch updates all three namespaces; remeasure a quiet
push before attributing a wall-time saving to this change. The first push after
that change still took over two minutes in the hook, with 86 close refs in the
snapshot. The rule scanned every receipt with a separate Git and jq process for
each pushed commit. The close-receipt check now reads all blobs in one batch and
memoizes the unpublished set per agent for the hook run. Its focused test creates
90 close refs, including another agent's receipts and a close already on
`origin/main`, and verifies that only this agent's unpublished tasks are
reported. PR #1108 merged 51 s after its push: branch compile took 36 s and
workflow compliance took 14 s. With the ref snapshot already fetched, a local
compliance run against its two commits took 3.9 s, and the standalone receipt
helper took 2.8 s including shell startup. These are separate measurements;
the full local finish remained over a minute, with a stale watcher restart and
a shared Cargo package-cache lock wait. All 30 gate tests passed in 43 s; the
90-ref fixture was the slowest because it launched Git twice per receipt.
Its setup now writes all receipt files and uses one `hash-object --stdin-paths`
and one `update-ref --stdin`, preserving the same cases.

## Finish avoids duplicate coordination (2026-10-31)

The measured finish for PR #1109 took 120.1 s, despite its 30 tests finishing
in 11.7 s. Each sync fetched origin and then primary parking fetched it again.
Parking now reuses that invocation's fetched snapshot; standalone parking and
the watcher still fetch. A finish also renewed an owned claim with hours left,
incurring a claim fetch, ownership lookup and push. It now keeps a matching
local agent/branch lease with more than 15 minutes left. Short, expired,
missing or mismatched leases still take the renewal/readoption path; close and
pre-push retain their authoritative remote ownership checks. Finish prints
elapsed seconds for lease, sync, gate, close, push and release to identify the
remaining cost. These changes need a new complete finish measurement.

PR #1110's measured finish was 107.8 s: lease 1 s, initial sync 11 s, gate
33 s, close 19 s, publication sync 8 s, push 15 s and release 15 s, plus
bookkeeping. The 31 gate tests took 11.5 s. The local gate was still invoking
the affected-crate selector three times, each rereading the diff and resolving
metadata. `--plan` now returns test owners, lint closure, root package and
focused root targets together; the gate consumes that single selection and
removes its second implementation of root-test selection. Full-workspace and
uncertain inputs still select `ALL`. Remeasure this gate before attributing
wall-time savings to the combined pass.

## Queue operations and release checks (2026-10-31)

The task-boundary implementation now reads claims in one `cat-file --batch`,
fetches branch tips with queue records, and batches generation/agent lookups.
Owned receipts use one main-tree listing and cache repeated ancestor checks;
settled receipts are cleared together. Windows calls the Git install's native
binary directly. Workflow checks reuse resolved Git directories instead of
starting a process for each operation marker.

Close publishes its receipt and retained claim/agent index in one atomic,
compare-and-swap push. The retained claim is a new blob, so a reassignment
during acceptance rejects the receipt publication too. Local ref updates still
use Git's locking and storage backend. Renew and release use ownership refs
from the queue snapshot rather than a second remote lookup. The owned-run
report fetches branch tips and receipts together and reuses the batch receipt
reader; a live run took 4.2 s. All 104 administration tests and 63 workflow
acceptance tests passed before the full branch gate.

Normal release cuts push the version bump and hand verification to GitHub.
The tagged suite gates publication, and the Windows, Linux CPU and macOS
engine artifacts run smoke missions and offline checks before upload.
`SUSI_RELEASE_RERUN_TESTS=1`,
`SUSI_RELEASE_CHECK_GPU=1`, cloud qualification, and non-cut invocations retain
the local gate. A cut reports verification as pending until GitHub finishes.
An installed release predating this change does not have this handoff.

The no-wait task finish also hands affected-crate tests and dependent Clippy to
the required GitHub branch check, after local formatting and before publishing.
The task's acceptance still runs locally before its close receipt is created.
The handoff is enabled only when main contains the branch-test machinery, the
remote is GitHub, and the branch has not changed CI, hooks or scripts; otherwise
the original local gate runs. No local proof stamp is written for deferred work.
`--wait`, `SUSI_LOCAL_GATE=full`, or `SUSI_GATE_NOCACHE=1` keep local checks.
This describes the implementation, not a measured sub-60-second full workflow.

On v0.21.32, the local cut returned in 56.4 s and the first release assets
appeared 10 min 1 s after the tagged workflow started. Linux ARM64 finished
28 s before Windows but waited behind the optional-platform matrix. Each
platform now publishes independently after the tagged suite and its own checks;
queued publishers serialize release creation without cancelling one another.
The release can temporarily lack platforms that are still building. The new
ordering still needs a real release measurement.

## Gate fixture and finish critical path (2026-10-31)

The complete finish for PR #1112 took 353.6 s: lease 1 s, initial sync 9 s,
gate 245 s, close 18 s, publication sync 17 s, push 37 s and release 12 s,
plus bookkeeping. The gate passed 552 tests; its test execution took 157.6 s.
This is the baseline for the next measurement, rather than a sub-60 claim.

Git-heavy task fixtures now disable automatic maintenance in their disposable
repositories. The pipeline-boundary fixture keeps real claim, close, release,
overflow refusal and publication checks, while seeding intermediate accepted
receipts in one atomic transaction. Its isolated execution fell from 103.6 s
to 12.2 s. Native Git also handles queue relay operations on Windows. Local
mirror changes use one Git transaction with expected old OIDs; a regression
check proves a newer ref rejects the entire transaction without partial writes.

Finish defers primary-checkout maintenance, skips duplicate target seeding and
uses the push hook's authoritative fetch instead of a second full publication
sync. If main advances during the gate, it integrates that fresh snapshot and
retries the push once. Network failures, changed HEADs, merge conflicts and a
failed optional regate still stop the operation. Normal sync reads its owned
lease from the fetched agent index instead of invoking task listing and renewal
unconditionally. The duplicate legacy definition that hid that path was removed.
Hermetic runs print setup, build/test, doc-test and cleanup timings; their
per-test output defaults to slow and failed results, retaining the full summary.

The focused workflow checks, the pipeline-boundary fixture and the atomic
mirror regression pass. A complete updated gate and finish measurement must
still establish the end-to-end result. These source changes do not update an
older installed release by themselves.

## Live-claim scans and scratch cleanup (2026-10-31)

PR #1115 passed 559 hermetic tests and merged. Its instrumented default-thread
attempt was interrupted; the clean eight-thread experiment took 471.8 s end to
end (gate 301 s, close 12 s, push 89 s, release 24 s). That experiment did not
meet the target and is not adopted as the default. It also forced the pre-push
clippy pass with the global cache-bypass flag; subsequent measurements keep a
fresh gate's successful proof available to the push hook. The same pipeline
fixture took 79.3 s alone on this resumed host, versus the earlier 12.2 s, so
host conditions materially affect these timings as well as concurrency.

The remaining live-claim check still launched Git and jq for each ref. It now
reads the fetched snapshot through one byte-counted `cat-file` batch and parses
it once, preserving ref-derived task IDs and handling multiline and malformed
blobs. A real 94-ref fixture checks duplicate ownership, expiry at the boundary,
an unrelated owner, and reuse of one snapshot across agents. Primary hooks defer
their fetches to finish's existing maintenance step. Hermetic service termination
and home validation remain synchronous; deletion of the invocation's validated
scratch paths runs with detached streams afterward. Push output streams while
the bounded retry retains its existing refusal rules.

## Reuse format proofs and bound identity scans (2026-10-31)

PR #1116 passed 81 affected tests and merged. The measured script/xtask finish
was 151.4 s: gate 74 s, close 12 s, push 21 s and release 11 s, plus entry,
sync and bookkeeping. This is a narrower workload than PR #1115's 559 tests,
so those end-to-end totals are not a controlled before/after comparison.
The stage timers identified 25 s spent formatting source the commit hook had
just checked, and a separate identity-hook measurement took 29.6 s.

The commit hook now records a format-only proof, which finish reuses for the
same source and formatter. The shared source stamp binds all indexed sources,
working edits, untracked names and contents, Cargo/Rust environment and external
Cargo configuration, and tool versions. Gate proofs additionally bind the
selected diff and full-gate policy. Only `.agents/tasks/` bookkeeping is exempt;
compiled identity, roadmap and evidence data remain inputs. This corrects the
old diff-only key: an upstream edit in another file could leave the branch diff
unchanged and incorrectly reuse the prior result. Older standalone script copies
without the helper run their checks without reusing a proof.

Identity checks read Git's worktree indirection files and local configuration
with shell builtins, falling back to Git for inherited or quoted values. One
scoped config read finds shared login names, and a NUL-delimited worktree listing
preserves spaces and relative Git paths. Normalized tokens detect equivalent
spellings such as `scan-native` and `SCANNATIVE`. The checkout's check measured
3.5 s after the initial change, versus 29.6 s before; host saturation later
made even tiny Git commands take around a second. A host snapshot recorded
100% CPU use while another full gate ran, with ample free memory and no disk
queue. The new complete finish remains to be measured.

The four source/format proof regressions and the existing performance checks
pass (72 total), as does the new identity-path case. A separate eight-thread
libtest experiment passed all 481 active gawd tests but took 428.5 s in the
saturated host; it is not adopted as the runner default. No sub-60 full Rust gate
or binary release is claimed from these experiments.

## Atomic branch publication and claim release (2026-10-31)

PR #1117 passed 85 xtask tests and all six identity tests. Its complete finish
took 272.6 s: gate 137 s, close 15 s, push 37 s and release 25 s, plus lease,
sync and bookkeeping. Reusing the commit hook's format proof took 3 s, versus
the earlier 25 s repeat check. Two separate hermetic runs still cost 38 s and
49 s for xtask and the focused root tests. Host load and the different test
selection prevent treating these totals as a controlled speedup comparison.

Direct, non-waiting finish now publishes its frozen branch head and releases
its claim in one atomic push. The transaction pins the claim, agent index and
accepted receipt to their observed object IDs; a concurrent reassignment or
receipt change rejects every update. The receipt retains its accepted source
head, scopes and acceptance time, with an additional publication revision so
even a same-head retry requires a real compare-and-swap update. Local mirrors
use Git's own ref transaction and never replace a newer concurrent snapshot.

Cloud relay, another queue remote, an already released claim and explicit
`--wait` retain the existing separate path. A rejected atomic push never falls
back to releasing ownership. Real bare-server regressions cover packed refs,
same-head republication, claim/index/receipt races and refusal to rewind the
code branch. PR #1118 passed all 92 affected tests (8.0 s test execution) and
merged with all required branch checks green. Its full affected gate took
23 s; finish took 62.025 s, including lease 1 s, sync 5 s, close 9 s and atomic
push/release 15 s, plus bookkeeping. The source commit took another 10.338 s.
The required remote checks completed 37 s after starting, and the merge landed
16 s after that. These are measured boundaries, not a sub-60 commit-through-
merge claim or a comparison with the original 552-test workload.

Two mixed-run experiments were also measured on the same checkout. Building
the selected package and focused root binaries separately, merging nextest's
binary metadata and executing their union passed 100 tests in 25.891 s for the
complete hermetic wrapper. The separate wrappers passed the same 100 tests in
27.202 s. A smaller shared-home wrapper passed 99 tests in 27.272 s; its test
selection differed by the experiment's own regression tests. Neither result
establishes a worthwhile, stable improvement. Both implementations were
discarded before publication; the existing Cargo selections, cleanup boundaries
and runner remain. No sub-60 broader Rust gate or binary release is claimed.

## Compiler-cache failure recovery (2026-10-31)

PR #1119 recorded the rejected mixed-run experiments and merged. Its measured
source-commit-through-finish duration was 84.521 s, and source-commit-through-
merge was 135.000 s. This documentation change also ran its 79-test acceptance;
it does not establish the larger Rust workload's duration.

A separate fresh diagnostic selected `susi-gawd`, `susi-harness`,
`susi-unsafe-ratchet` and `xtask`, including more harness tests than the earlier
559-test run. Clippy passed in 96 s; the test build then failed when the shared
sccache server reset its connections (224.847 s total, no tests executed).
An invocation-only compiler-cache bypass built the test binaries in 103 s;
nextest ran 1,039 tests in 87.945 s: 1,038 passed, one failed and three were
skipped. The failure was the format-proof fixture inheriting the diagnostic's
`SUSI_GATE_NOCACHE=1`, rather than isolating the cache policy it tests. That
fixture now removes the caller's override; its focused check passes with the
outer bypass explicitly enabled. The clean fresh rerun passed all 1,039 tests
in 84.122 s, plus doc tests; its complete gate took 105.782 s with no compiler
cache. Three tests remained skipped. PR #1121 merged with the 92-test affected
gate also green: source commit 19.603 s, finish 54.339 s, and commit through
merge 150.000 s. The full commit-through-merge target remains unmet.

Workflow and standalone hermetic builds now default
`SCCACHE_IGNORE_SERVER_IO_ERROR=1`, while preserving an explicit `0`. This uses
sccache's own local-compiler fallback after an interrupted compilation, retaining
compiler failures. It does not suppress cache startup errors or restart the
shared server. An isolated TCP fixture against installed sccache 0.19.0 sent
`CompileStarted` and reset its connection: strict mode failed with exit 2,
fallback compiled valid Rust with exit 0, and invalid Rust still failed with
exit 1. No machine-wide configuration was changed for this recovery.

## Audit history traversal and verification setup (2026-10-31)

The clean diagnostic's slowest cases were post-merge verification (71.358 s)
and the in-flight queue boundary (67.619 s). The verification fixture now keeps
one real claim/acceptance lifecycle and seeds its other accepted records,
publishing merger attestations in one real atomic transaction. All checks of
success, failure, host-specific skipping and retry remain. Its initial focused
run passed in 7.43 s; this isolated result is not comparable with the loaded
whole-suite duration.

Inspection also found a production multiplier: `tasks audit` ran both Git
ancestry checks twice for each attestation, once for the real partition and
again for the forged partition. This checkout held 557 merge refs, allowing
over two thousand ancestry subprocesses per audit. The audit now walks main's
full parent history once, checks each unique full-ID pair in memory and retains
all task records sharing a pair. Abbreviated IDs and legacy ref spellings still
go through Git. Each audit reads a new history snapshot; no ancestry proof
persists across main advancing.

The real-Git attestation regression now also exercises duplicate records, a
previously unpublished merge reaching main, legacy abbreviated IDs and a head
that is on main but lies after the attested merge. Live before/after audit and
complete gate measurements must still establish this change's total savings.

## The gate's lint and doc-tests overlap the test run; one queue push per finish (2026-10-31)

Measured in a cloud session's clone (Linux, 4 vCPU, the dev binary, no
compiler cache, no primary watcher), before the changes below:

| step | measured | note |
| --- | --- | --- |
| `git fetch origin` (156 local refs, 2 700 remote) | 0.4–0.6 s | the remote round trip itself is cheap here: the 5–11 s syncs in the sections above were the Windows host's |
| `git ls-remote origin <one ref>` | 0.4 s | the lease check |
| `scripts/fetch-workflow-refs.sh` (the push guard's fetch) | 0.4–0.7 s | |
| `susi workflow check` | 1.1 s | |
| `susi tasks list` | 0.5 s | |
| `cargo fmt --all --check`, uncached | 4.7 s | under a cold build's load; the format proof (above) skips it |
| `cargo metadata --no-deps` | 0.13 s | the gate's scope resolution |
| one relayed queue write (`.github/workflows/queue.yml` run) | 9–21 s run, plus ~3 s until it starts and up to 3 s of client polling | from the Actions API: 15 runs of the previous session, created → updated |

So in a cloud session the finish's fixed cost is not the network but the
**relay**: every write to the queue's refs is one Actions run, and a finish
made two of them in a row — the close receipt, then the claim's release
(21:52:35 and 21:53:01 in the previous session's run list: 26 s apart, the
second waiting on the first). The lease, sync and push legs together are
under 3 s here.

Two changes:

- **The gate's clippy and doc-tests run alongside the tests, not after them**
  (`scripts/parallel-workflow.sh` `gate_lint_and_test`,
  `scripts/check-hermetic-tests.sh`). Every cargo of the gate builds into
  the worktree's one target, and cargo's lock on the build directory
  serializes the *builds* — but the test run holds no lock. The test steps
  start first, clippy two seconds later: the test build takes the lock,
  clippy waits for it, and then compiles while the tests execute. The
  doc-tests do the same inside the hermetic runner (`cargo test --doc`
  started after nextest, waiting on the same lock). Clippy's and the
  doc-tests' output is held and printed whole when they end, so a lint
  failure does not interleave with the test lines; both verdicts are waited
  for. When clippy wins the lock instead, the order is the old one and
  nothing is lost. `SUSI_GATE_SERIAL=1` restores the one-after-the-other
  gate for a machine whose timing-sensitive tests cannot spare the cores;
  `SUSI_GATE_LINT_DELAY` (2) is the head start the test build gets.
  `tests/worktree_target_dir.rs` and `xtask/tests/ci_workflow_perf.rs` pin
  the shape; the seeded-target fixture no longer assumes clippy logs before
  the tests.
  Measured on this diff (susi-gawd, xtask and one root test binary to test;
  susi, susi-daemon, susi-gawd, susi-gawd-a2a and xtask to lint; `tasks.rs`
  touched before each run so susi-gawd and its dependents recompile both
  times; `SUSI_GATE_NOCACHE=1`; 4 vCPU, no compiler cache):

  | gate | clippy | tests | wall |
  | --- | --- | --- | --- |
  | serial (`SUSI_GATE_SERIAL=1`) | 7 s | 12 s (build and run 10 s, doc-tests 2 s) | 24 s |
  | overlapped | 13 s, ending with the tests | 15 s (doc-tests inside) | 19 s |

  The lint's 7 s and the doc-tests' 2 s left the critical path; the tests
  took 3 s longer sharing four cores with clippy. The first, cold run of the
  same gate spent 210 s in clippy check-building the dependents' targets
  (the root package's hundred test binaries among them) — the part a seeded
  target or a compiler cache carries, and where a 28-core host gains most
  from the overlap.

- **The close receipt rides the release's push.** Without `--wait`, `finish`
  publishes the branch and releases the claim seconds after the close, so the
  receipt no longer takes a push of its own: `finish` sets
  `SUSI_CLOSE_DEFER_RECEIPT=<id>@<worktree>` and `tasks close` leaves the
  receipt to the publication, and `SUSI_RELEASE_PUBLISH_RECEIPT` has `tasks
  release` build it from the committed completion (`.agents/tasks/done/
  <id>.json`, the claim's scopes) and create it in the same compare-and-swap
  push that deletes the claim and the agent index — an empty lease on the
  receipt, so one that appeared meanwhile refuses the whole push. On a laptop
  `scripts/publish-task-branch.sh` does the same in its atomic branch push.
  The claim is held until that transaction lands, so nothing can take the
  task in between, and ownership is never gone without the debt on the
  remote; a finish that dies after the close leaves the task closed under
  its claim, which the next finish publishes, and a plain `tasks release`
  of such a task is refused as an unpublished close is. In a cloud session
  this is **one relayed Actions run per finish instead of two**; on a laptop
  one push instead of two. `--wait` keeps the close publishing first (its
  wait reads the debt from the remote). Pinned in `admin::tasks::tests`
  (`a_deferred_receipt_is_published_with_the_release_in_one_push`) and
  exercised end to end by `tests/parallel_workflow.rs`, whose finish runs
  the real binary against a bare remote.

**Measured end to end** (this cloud session, 4 vCPU, the dev binary, the
gate forced with `SUSI_GATE_NOCACHE=1` so it is inside the number): `susi
workflow finish` for the change above took **62.2 s** — lease 1 s, sync 0 s,
gate 39 s (581 susi-gawd and xtask tests in 9 s, then 67 process-spawning
root tests in 25 s, clippy's 2 s and the doc-tests inside those), close 1 s
(the receipt deferred: no relay), push 4 s (the hook's fetch and compliance
check; clippy skipped on the gate's proof), release 17 s: the one relayed
Actions run, carrying the receipt, the claim and the agent index (the run
itself took 10 s, created → updated). The fixed cost outside the gate is 23 s,
17 s of it that single relay; the previous session's finish paid two. The
earlier attempt the same hour failed its gate on a fixture that inherited the
outer `SUSI_GATE_NOCACHE=1` (`tests/parallel_workflow.rs` now drops it, and
the hermetic runner unsets it) and on one flaky xtask case
(`normal_sync_finds_its_owned_claim_without_remote_renewal_lookup`, green on
nextest's retry). The merge itself remains the machinery's: PR #1187 opened on the
push at 23:17 and was merged by 23:19, about two minutes after the push
(Compile Check and Workflow Compliance on the branch run, then the merge job).

## The cloud target seed (2026-11-01)

The cold numbers of a cloud session, from the session above: the background
build of susi took about 15 minutes (21:58 → 22:13 on 4 vCPU, ~700 registry
crates and the ~80 workspace crates), and the first gate's clippy of a changed
crate's dependents 210 s, check-building the dependency tree in check mode
for the first time. A laptop never pays either: its worktrees copy the
primary's snapshot (`scripts/seed-target.sh`). A container has no primary.

So the snapshot is published. `.github/workflows/target-seed.yml` builds
`origin/main` as a container does (the pinned toolchain; the cargo
configuration `scripts/cloud-onnxruntime.sh` writes, whose rpath is in every
unit's rustflags; the clone at `/home/user/susi`), `cargo build --workspace
--all-targets` and `cargo clippy --workspace --all-targets` — the build and
the check-mode units the gate reads — then `susi workflow seed-target
--export` copies the target and strips it exactly as the refresh strips the
primary's (registry and git units only, no `incremental/`, a `.susi-seed`
mark), and the result goes up as assets of the rolling `target-seed`
pre-release: gzip'd tar in parts under 1.9 GB (the asset limit is 2 GiB), a
sha256sum manifest, named by the key `scripts/cloud-target-seed.sh --key`
computes — the external packages of `Cargo.lock`, the toolchain pin, `rustc
-vV`, the machine, the `[profile.dev]` tables and the ONNX Runtime
configuration block — and a `linux-x86_64-latest.manifest` naming the newest.
The workflow runs when a key input changes on `main`, weekly, or on demand,
and costs one checkout when the key's snapshot already exists; two keys are
kept, for branches behind `main`.

The session-start hook fetches it (`scripts/cloud-target-seed.sh`, before
`cargo build -q`, inside the one subshell that holds the clone's build lock),
verifies every part, unpacks into an empty `target/` and leaves one that holds
a build alone; no snapshot for the exact key takes the newest, and cargo
recompiles the crates that differ. The registry-only part of this session's
target measured 4.9 GB uncompressed, so expect about 1.5–2 GB to download.

**Measured, in this session's container** (a fresh clone beside this
checkout; 4 vCPU; the numbers are what the hook pays before a session's first
`susi workflow check`, then what the gate's dependents clippy pays):

| step | cold | seeded (first snapshot) | note |
| --- | --- | --- | --- |
| `git clone` | 15 s | 15 s | |
| `scripts/cloud-target-seed.sh` | 0 s (nothing published) | 54 s | 1.45 GB in one part, verified and unpacked: 2 957 files, 4.5 GB |
| `cargo build` | 742 s | 626 s | |
| dependents' clippy (`-p susi -p susi-daemon -p susi-gawd -p susi-gawd-a2a -p xtask --all-targets`) | 232 s | 207 s | |

The first snapshot bought almost nothing, and cargo's own output said why:
the seeded build began with `Compiling unicode-ident`, `proc-macro2`, `serde`
— every registry crate again. The units' hashes matched (the same
`unicode-ident-<hash>` directories in the snapshot and in a target this
container built itself), so the metadata was right; their fingerprints were
not. The first guess — `CARGO_INCREMENTAL=0`, which the publishing job had and
the container does not — was wrong: a republish without it measured the same
(seeding 65 s, build 625 s, dependents' clippy 211 s, `Compiling unicode-ident`
first again). The fingerprint files settled it. The snapshot's
`lib-unicode_ident.json` and this container's agree on every field — `rustc`,
`profile`, `config`, `rustflags`, `features` — except `path`: cargo hashes a
registry unit's source file by its absolute path, which lives under
`CARGO_HOME`, and the job's was `/home/runner/.cargo` where the container's is
`/root/.cargo`. (Workspace members are hashed relative to the workspace root,
which is why the laptop's seeding, within one machine, never met this.) The
job now fetches the registry into `/root/.cargo` and builds under it, like the
container. Re-measured against that snapshot, in the same clone, same host:

| step | cold | seeded (snapshot built under `/root/.cargo`) |
| --- | --- | --- |
| `scripts/cloud-target-seed.sh` | — | 68 s (1.45 GB, 2 957 files) |
| `cargo build` | 742 s | **93 s** — the first unit compiled was `susi-vendor-serde-json`: workspace crates only |
| dependents' clippy (the five-package `--all-targets` lint) | 232 s | **35 s** |

So a cloud session's first build is 161 s instead of 742 s including the
seeding, and the gate's dependents clippy pays for workspace check units only.
The publishing run itself takes about 18 minutes (13 min build, 3 min export
and upload), once per key.

The first measurement also found the hook deriving the release URL from a
clone URL ending in `.git` (a hand clone's; the harness's does not), looking
under a path that does not exist and falling back to a cold build; fixed, with
`--url` to show the resolved source.

Fingerprint parity across machines, then, needs the same `CARGO_HOME` as
well as the same compiler, profile, rustflags and `ORT_LIB_LOCATION`; the
checkout's own path does not matter for the kept units, and a build script's
recorded output directory is rewritten by cargo. A unit whose fingerprint
does not match is simply rebuilt, which is what the first two snapshots
showed.

## What is not fixed here

- **`-p <crate>` builds unify features differently from `--workspace`**
  (`tower`, `tokio-stream`), so a `-p susi` gate recompiles tower, axum and
  their dependents in a seeded target. Cargo's
  `feature-unification = "workspace"` is still unstable on 1.99; the cache
  above serves those variants after the first worktree compiles them.
- **Windows file I/O.** The fixture-copying tests are 6× slower per test than
  on Linux whatever the thread count; the exclusion above recovers part of
  that, a Linux build host the rest.
- **The release's slowest legs** are macOS and Windows runners; under 15
  minutes needs warm dependency caches those tag runs cannot read (the
  repository is at its 10 GB Actions cache quota) or larger runners
  (`vars.CI_RUNNER`).

## Release legs, measured on v0.21.27 (2026-10-31)

The tag-to-published path of release.yml, from the run's own job timings:

| step | v0.21.27 | note |
|---|---|---|
| verify the tag | 13 s | |
| full suite on the tagged commit | 7 min | reused the post-merge suite |
| build linux-x86_64 / aarch64 | 9 / 10 min | `cargo build --profile dist` 8.3 min cold, mold |
| build linux-x86_64-cuda | 16 min | |
| build windows-x86_64 | 28 min | `cargo build --profile dist` 23.6 min cold, harness 2.9 min, saving the target cache 1 min |
| build macos-aarch64 | cancelled after 1 s | unsupported runner label; fixed on main the same evening (98a93b3f9) |
| publish | skipped | a cancelled leg cancels the run, and a cancelled run publishes nothing (by design, tests/release_workflow.rs) |

So v0.21.27 was tagged but never published, and the wall time of a release is
the Windows leg. Two findings:

- **No release leg has ever hit its compiler cache.** sccache answered 0 of 892
  compiles on the Windows leg (0 of its C/C++ ones too). A GitHub Actions cache
  written by a run on a tag ref is readable only by that tag; the next tag
  starts cold. Only caches written on `main` are shared, and nothing on `main`
  builds the dist profile. The legs no longer save their target directories
  (`cache-targets: "false"`): the ~1 GB a tag run wrote per leg was never read
  again and only evicted Test's shard caches, which carry every pull request
  and already hold 9 of the repository's 10 GB.
- **The Windows leg is a cold 4-core MSVC build of ~700 crates with thin LTO.**
  Nothing in the workflow shortens it without a readable cache. The levers that
  would are a decision, not a tweak: a warm dist cache written on `main` (about
  2 GB per leg against the full quota, so Test's shards would have to shrink),
  a larger Windows runner (paid), or cross-compiling the Windows binary from a
  Linux leg (`cargo-xwin`, 8-9 min there), which has not been tried against the
  native dependencies (ort, candle) this binary carries.

The local cut (`susi admin release --cut patch`) is measured in the next
section when the next release is cut.

### Windows release cache experiment (2026-11-01)

v0.21.29 published 25 min 18 s after the release workflow started. Its
Windows engine took 20 min 21 s, its harness another 2 min 31 s, and the
publish job 41 s; the full tagged-commit suite ran in parallel. A snapshot of
the repository's Actions caches held about 10.65 GiB in 926 entries, many
scoped to that single tag. Such entries cannot warm the next tag.

`release-warm.yml` now builds the Windows dist dependency graph after a green
main Test run and saves its dependency-only target under the same rust-cache
key that the Windows release restores. Runs finish rather than being cancelled
by every new main commit. The release reads that cache but writes no tag-scoped
rust-cache or GitHub sccache entries. Other platform legs stay cold; the warm
cache competes with Test for the repository's cache quota. Its size, hit rate,
impact on Test caches, Windows time and complete release time must be measured
on the first subsequent warm release; these changes do not establish a
sub-60-second release.

The warmer and every release leg compile the engine and the harness in one
`cargo build`. A second cargo invocation waits on the target-directory lock,
so the harness link (2 min 31 s on the v0.21.29 Windows leg) used to start
only after the engine had finished.

`publish` needs the suite and the contracted legs (linux-x86_64 and Windows)
only. arm64, macOS and CUDA upload when they finish, and `publish-extra`
attaches those assets to the release that already exists. On v0.21.29 the
wall was Windows at 24 min, so waiting for the rest added nothing; once the
Windows cache hits, macOS (~19 min) and CUDA (~17 min) would have been the
wait in front of binaries that were already built.

## The loop's scripts as susi subcommands (2026-10-31)

`scripts/seed-target.sh`, `scripts/park-primary.sh`, `scripts/prune-worktrees.sh`
and `scripts/susi-worktree.sh` are stubs now: each finds a susi that carries
its verb and execs it (`susi workflow seed-target`, `park-primary`, `prune`,
`start`), and the work is Rust that runs `git`, `cargo` and the copy tool
directly. The reason is robustness and portability, not speed: an installed
`susi.exe` now does all of it without Git for Windows' MSYS userland on PATH,
and the hooks and older branches keep working through the stubs. Nothing in
the loop was made faster or slower on purpose, and no linker setting moved.

Re-measured after the ports, in a cloud session's clone (Linux, 4 vCPU, the
dev binary, no primary watcher and no remote reads to wait on):

| Step | Measured | Note |
| --- | --- | --- |
| `susi workflow check` | 0.4–0.9 s (three runs) | the primary sync runs in process now instead of through `park-primary.sh` |
| a stub's susi probe (`<verb> --help`) | 13 ms here; 0.23 s release / 0.45 s debug measured on the Windows host for the hook stubs | one extra susi start per script call, where a script is still the caller |

The 13 s for `susi workflow check` in the table at the top was measured on
the Windows host with its remote reads (`gh`, `git ls-remote`), which dominate
it; the port touched none of those, so that number stands until it is
re-measured there.

## Consolidating finish/sync network trips, auto-gc guards and process sweeps (2026-11-01)

Profiling the Windows post-task boundary identified four remaining sources of
serial latency outside compilation:

- **Synchronous Git auto-gc stalls (20–24 s per fetch/commit).** Neither
  `ensure_infrastructure` nor `parallel-workflow.sh` disabled `gc.auto` or
  `maintenance.auto` in real worktrees, so periodic `git fetch` and `git commit`
  calls inside `sync` and `finish` blocked on foreground `git gc --auto`.
  `ensure_infrastructure` now reads `core.hooksPath`, `merge.ledger.driver`,
  `gc.auto` and `maintenance.auto` in a single `git config --get-regexp` call
  (1 subprocess instead of 4) and sets `gc.auto=0` and `maintenance.auto=false`
  when absent; `start_worktree` and `parallel-workflow.sh` enforce the same.
- **Single-fetch `sync` and `finish` prefetch.** Normal `susi workflow sync`
  previously ran up to three separate `git fetch` round-trips (branch tips,
  `renew_owned_claims` and `check-owed-runs.sh`). `sync_fetch_origin` now
  fetches `+refs/heads/*:refs/remotes/origin/*`, `+refs/claims/*:refs/claims/*`,
  `+refs/claim-agents/*:refs/claim-agents/*` and `+refs/closed/*:refs/closed/*`
  together in one locked `git fetch`, passing `SUSI_REMOTE_CLAIM_FETCHED=1` and
  `SUSI_OWED_REFS_FETCHED=1` to the downstream steps. `finish` similarly runs
  `prefetch_finish_refs` once at entry (preserving the pre-fetch local claim
  snapshot in `SUSI_PREFETCH_LOCAL_CLAIM_OID` and `SUSI_PREFETCH_LOCAL_CLAIM_BODY`
  for `renew_or_readopt`) and sets `SUSI_FINISH_ORIGIN_FETCHED=1` and
  `SUSI_CLOSE_QUEUE_SYNCED=<id>@<worktree>` so `lease`, `initial-sync` and
  deferred-receipt `tasks close` share that single remote snapshot instead of
  making three separate SSH round-trips.
- **Fast process-sweep precheck on Windows** (`scripts/kill-leftover-service-runs.sh`).
  Each `finish` and hermetic test run invoked `powershell.exe -NoProfile` and
  `Get-CimInstance Win32_Process` (~2 s per call) even when no `susi.exe`
  process was running. A `tasklist.exe /FI "IMAGENAME eq susi.exe" /NH` precheck
  now exits in ~60 ms when no matching process exists, spawning PowerShell CIM
  inspection only when a `susi.exe` instance is actually present.
- **Shell-builtin path walking and direct stdin hashing in `gate-input-stamp.sh`.**
  Replaced per-file `sha256sum | awk` pipelines with `sha256sum <file` and
  replaced the parent-directory `dirname` subprocess loop with parameter
  expansion (`${cur%/*}`). Combined with direct ref/config setup in
  `ProofRepo` and `PublicationRepo`, the 82-test `xtask::ci_workflow_perf`
  acceptance suite dropped from 62.8 s to 57.5 s on this Windows host.

## Push to merge: the merge job moves into the Test run (2026-11-01)

The loop after the work is done — `finish`, the branch run, the merge, the next
claim — reviewed for waits and bloat, with the time of each piece measured from
the Actions API on the pushes of this session:

| piece | before | after | what it was |
| --- | --- | --- | --- |
| required checks green (docs-only push) | 25 s | 25 s | Format Check, Workflow Compliance, cargo deny, Compile Check |
| required checks green (Rust push) | 77–79 s | 79 s | Compile Check: checkout 10 s, resolve 11 s, cache restore 16 s, clippy 30 s |
| Repository Invariants | 93–110 s | 31 s | `check-roadmap-verdict.sh --all`: 89 s → 3 s |
| last needed job → merge landed | 14–22 s after the *whole run* | 22 s after the *needed jobs* | a second runner through the workflow_run hook → the run's own `merge` job |
| push → merged, scripts and a test file (PR #1202) | 110–170 s | 88 s | Compile Check 68 s lints the root test target; merge job started 2 s after it, landed 18 s later |
| push → merged, Rust (PR #1200) | 110–170 s | 106 s | Compile Check is the pole |
| `finish`, docs-only | 22–27 s | 22 s | gate 4 s, close 1 s, push 3 s, one relayed release 12–17 s |
| `finish`, Rust change | 57–62 s | 57–121 s | the gate (29–70 s: clippy of the dependents cold after main moved); a push refused as behind main costs a merge and a second push (24 s) |
| next claim (cloud) | 17–34 s | 17–26 s | one relayed Actions run (9–21 s) plus one `ls-remote` for all leases instead of one per lease |

Three things were waits for nothing:

- **The merge waited for the slowest job of the run, not the required checks.**
  auto-merge.yml merged on `workflow_run: Test completed`, which fires when
  every job has finished — Repository Invariants at 110 s, which the ruleset
  does not require — and then started another runner (its own checkout, probe,
  merge: 22 s). Test's `merge` job, which already merged dispatched retests,
  now merges pushed branches too, the moment the jobs it needs are green, in
  the same serialized queue (`auto-merge-integration`); the hook remains the
  backstop for a run the job could not merge. A pushed run keeps
  `SUSI_NO_RESYNC`: a stale head is handed to a reconcile pass, which gates it
  in place, instead of an update-branch and a second run.
- **The invariants' 89 s was one awk.** `check-reachability.sh --batch`
  character-cleaned every line of every production file and ran every
  symbol's regexes on each, under gawk. Lines are cleaned only when they hold
  a delimiter, the per-symbol regexes run only on lines a combined alternation
  matches, and mawk is preferred where installed: 3.8 s → 1.8 s for the same
  19 checks under mawk here, 89 s → 3 s in the job. The verdict checker's
  function index uses grep instead of gawk's three-argument `match`, so the
  whole check runs under any awk (2.2 s for 131 verdicts here; it used to fail
  under mawk before it began).
- **The relay client paid a round trip per lease** before relaying (to tell a
  lost lease from a refused push); one `ls-remote` now answers for all of them,
  as the read-back after the relay already did.

What stands between a Rust push and 60 s, and is a decision rather than a tweak:

- **Compile Check on a hosted runner is ~79 s**: 10 s of checkout (full history
  for the merge base), 11 s resolving the affected crates (the pinned toolchain
  downloads for the first `cargo metadata`), 16 s restoring the target cache,
  30 s of clippy on an sccache hit. A self-hosted or larger runner
  (`vars.CI_RUNNER` exists for it) would take most of it; dropping the check
  from the required set would take all of it and trust the local gate plus
  main's post-merge suite, which already runs everything and keeps main red
  until fixed forward — the local gate can be bypassed with `--no-verify`, which
  is what the server-side check guards.
- **The local gate itself**: clippy of a changed crate's dependents is the
  gate's pole (30–70 s on 4 vCPU when main moved under the branch); the seeded
  target removes the cold part, not the incremental one.
- **Two relayed Actions runs per cycle in a cloud session** (the release in
  finish, then the next claim: ~15 s each, GitHub's own scheduling). Folding
  the release into the next claim's transaction, or giving the queue's refs a
  branch-shaped home the proxy would push directly, would remove them; both
  change the queue's contract and are the operator's call.
- **A push refused as behind main** after a long gate costs a merge and a
  second push (24 s in #1200). The retry is right; the cost is the gate's length.
