# Agent-operated local Windows channels

Agents build development executables with `cargo local-build debug` in a
reserved worktree. For production, `cargo local-release qualify` verifies the
accepted integration head and returns the release ID. Agents promote the
qualified ID with `cargo local-release promote <id>`, launch it with
`cargo local-release run`, and restore its verified predecessor with
`cargo local-release rollback`. Use `cargo local-release status` to inspect
both release IDs. These commands work locally.

For Windows user deployment, the agent invokes `cargo local-release install-current`
after verified promotion or rollback. This command installs and checks the promoted
ID for ordinary launches. For a clean machine, `cargo local-release export-qualified
<id> <new-absolute-directory>` exports the admitted release and its qualified
predecessor when available. The agent transfers the bundle and release IDs through
its trusted connection and invokes the current bundle's `susi.exe install-release
--id <id> --previous <previous-id>`. Omit the predecessor argument for the first
qualified release. The target verifies the executable, evidence, notices and SBOM
offline. The producer retains the full qualification log.

The user can then give Susi a goal with `susi "<what you want done>"` in a new
terminal; the core CLI takes one-shot goals, and the interactive conversation
lives in the Susi desktop app. The **Susi (local production)** Start Menu entry
starts bare `susi` in the channel workspace, which prints usage and exits; it
and the `susi` command use the same installed selector. Its atomic update
selects an immutable executable for each new launch. Running processes continue
from their original slot. Repeating an interrupted installation recovers its
recorded transition. The agent can inspect the deployment with
`susi install-release --status`.

A Windows distributor migrating the flat Commercial v3.1.3 installation runs
the updated migration controller before removing the old programs:

```text
susi-rollback-manager.exe install-release --import-legacy --id <trusted-current-core-id> --controller-sha256 <trusted-controller-sha256>
```

Place the qualified current core in the adjacent `slots/<trusted-current-core-id>`
directory. The controller authenticates the original published Windows v0.21.51
engine by its pinned executable SHA-256 and size, captures it in its own slot,
and keeps the verified management executable separate from both runtime engines.
The commercial installer installs the corresponding desktop for each selection.
Stop the running Susi programs before switching. The installed `susi install-release
--rollback --expect <old-id>` restores the original engine; `susi install-release
--promote <current-id>` reselects the newer engine. Repeating either targeted
operation keeps that version selected. All selections use the same user profile.

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
with `susi-commercial workflow seed-target --refresh --force`; setting
`SUSI_SEED_AUTO_REFRESH=1` before starting the primary watcher explicitly restores
automatic refresh. Restart an already-running watcher to load that policy.
