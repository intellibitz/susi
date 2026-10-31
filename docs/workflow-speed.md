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
- **Thin LTO for the published binaries** (`CARGO_PROFILE_DIST_LTO=thin`,
  16 codegen units, set in `release.yml` and `dev-release.yml`). Under
  `[profile.dist]`'s fat LTO the single `susi` unit ran 988 s on one thread
  after the ~700 dependencies finished; thin LTO runs it on every core.
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

Release smoke missions and the seven isolated offline checks run in parallel.
The release gate builds only `susi`, reuses successful main evidence for
identical code trees with task-queue-only differences, and marks its final push
so the pre-push hook does not repeat clippy. These are source changes: a stale
installed release does not gain the new task/release implementation until it
is updated. No new complete finish or binary-release duration is claimed here.

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
