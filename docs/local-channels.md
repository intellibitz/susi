# Agent-operated local Windows channels

Agents can build, install and exercise Susi without waiting for GitHub. From
their own warm worktree, run `scripts/susi-local.ps1`. The default `Build`
action uses the existing incremental development profile and installs under
`%LOCALAPPDATA%/SusiLocal/dev`; it never replaces the public production install.
`-Channel production` uses an optimized local build and a separate local
production installation. Neither channel needs a release tag or remote CI.
It keeps inference dependencies optimized at O3 and optimizes Susi at O1 using
process-local Cargo development-profile overrides. Incremental compilation,
debug assertions, overflow checks and line backtraces remain enabled; this is
distinct from the public release profile. Whole-program LTO is disabled. Cargo
fingerprints the optimization differences while reusing unchanged dependencies
in the same worktree cache, without duplicating the target directory. All three
overridden environment settings are restored, including on build failure.

Each candidate is copied to a directory named by its SHA-256. Before activation,
the script checks its checksum and version, then runs a real offline file goal
in a fresh temporary workspace. Both the visible answer and the governed mission
record must contain the fixture's random content. Failure leaves the current
channel intact. Evidence stays in the printed temporary directory.

`-Action Run` launches the activated channel with its own Susi home and port
offset (development 200, local production 300). `-SusiArgs` passes arguments to
that executable. `-Action Rollback` selects the previous installed build without
recompilation; `-Action Status` shows the activation record. Agents operate these
commands; the CEO supplies goals. Repository acceptance and publication gates
still apply to publishing code, independently of this local verification loop.

The local smoke check proves the governed file journey, not every model or cloud
provider. Agents must also run the acceptance checks relevant to their change.
The script preserves the current Cargo cache and capabilities. It does not
silently delete targets, change features, or rebuild every test binary.

Automatic background snapshot compilation is off by default. Existing snapshots
still seed new worktrees. Agents can refresh during an idle maintenance window
with `susi workflow seed-target --refresh --force`; setting
`SUSI_SEED_AUTO_REFRESH=1` before starting the primary watcher explicitly restores
automatic refresh. Restart an already-running watcher to load that policy.
