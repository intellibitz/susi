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

## What is not fixed here

- **One shared target serialises every agent's build.** Cargo locks the
  build directory, so two worktrees never compile at once, and the guard
  rebuilds a crate whenever the last worktree to build it had different
  content. Per-worktree targets with a content-addressed compiler cache
  (sccache, local disk) would let N agents build at once and still share
  the ~630 registry crates; that is a separate change to
  `scripts/link-shared-target.sh`, `scripts/target-guard.sh` and the tests
  that pin them.
- **`-p <crate>` builds unify features differently from `--workspace`**
  (`tower`, `tokio-stream`), so the first `-p susi` gate after a workspace
  build recompiles tower, axum, rmcp, candle and fastembed. Cargo's
  `feature-unification = "workspace"` is still unstable on 1.99; the variants
  stay cached once built.
- **Windows file I/O.** The fixture-copying tests are 6× slower per test than
  on Linux whatever the thread count; the exclusion above recovers part of
  that, a Linux build host the rest.
- **The release's slowest legs** are macOS and Windows runners; under 15
  minutes needs warm dependency caches those tag runs cannot read (the
  repository is at its 10 GB Actions cache quota) or larger runners
  (`vars.CI_RUNNER`).
