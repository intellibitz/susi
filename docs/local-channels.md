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
in the same worktree cache, without duplicating the target directory. This warm
incremental path invokes rustc directly, independently of a shared compiler-cache
server. The compiler-wrapper and profile environment settings are restored,
including on build failure; global Cargo configuration remains unchanged.

Each candidate is copied to a directory named by its SHA-256. Before activation,
the script checks its checksum and version, then runs a real offline file goal
in a fresh temporary workspace. Both the visible answer and the governed mission
record must contain the fixture's random content. Failure leaves the current
channel intact. Evidence stays in the printed temporary directory.

Build automatically installs **Susi (local production)** or **Susi
(development)** in the Windows Start Menu. Its launcher is copied into the
installed channel and keeps working if the source worktree is removed. The CEO
opens the app and enters a goal; agents handle builds and verification.

On launch, the channel adds the canonical model cache to its read-only discovery
paths. It does not copy model weights or redirect writes into that cache. Stored
provider API keys, base URLs and model preferences from canonical `cloud.env`
are read into the child environment only when no existing process value wins;
arbitrary command and home settings are excluded. The file is never copied or
modified, and imported environment values are restored after exit. New downloads
and mutable runtime state remain separate for each channel.

`-Action Run` launches the activated channel with its own Susi home and port
offset (development 200, local production 300). `-SusiArgs` passes arguments to
that executable. `-Action Rollback` selects the previous installed build without
recompilation; `-Action Status` shows the activation record. Agents operate these
commands; the CEO supplies goals. Build and rollback refuse activation while
this channel still has running processes, so agents finish goals and stop its
daemon before switching. A rollback verifies the previous content-addressed
executable before running it. Repository acceptance and publication gates
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
