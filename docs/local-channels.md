# Agent-operated local Windows channels

Agents build development executables with `cargo local-build debug` in a
reserved worktree. For production, `cargo local-release qualify` verifies the
accepted integration head and returns the release ID. Agents promote the
qualified ID with `cargo local-release promote <id>`, launch it with
`cargo local-release run`, and restore its verified predecessor with
`cargo local-release rollback`. Use `cargo local-release status` to inspect
both release IDs. These commands work locally.

The legacy Windows helper `scripts/susi-local.ps1` supports the development
channel only. Its default `Build` action installs the incremental development
executable under `%LOCALAPPDATA%/SusiLocal/dev`. Whole-program LTO is disabled. Cargo
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

`-Action Run` launches the development channel with its own Susi home and port
offset 200. `-SusiArgs` passes arguments to
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
