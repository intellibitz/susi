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
push before attributing a wall-time saving to this change.

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
