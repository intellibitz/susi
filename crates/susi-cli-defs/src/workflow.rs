//! `clap` definitions for the `workflow` command family, moved verbatim from
//! `src/cli/workflow_cli.rs` (D-SP-4 Stage 4d). The handlers stay in the root
//! crate, which re-exports these types from `src/cli/workflow_cli.rs`.
//!
//! An item marked `#[allow(missing_docs)]` keeps undocumented fields (or is
//! hidden) on purpose: clap turns every `///` comment into `--help` text, and
//! this move must leave the visible help byte-identical.
use clap::Subcommand;

/// Subcommands of `susi workflow`: the agent worktree workflow.
#[derive(Debug, Subcommand)]
pub enum WorkflowCommands {
    /// Fetch and merge origin/main into a clean agent worktree.
    Sync,
    /// Verify, close, push, publish the close receipt and release ownership.
    /// finish returns once the close receipt is published; the branch reaches
    /// main through `cargo local-workflow ship`, not a hosted run.
    #[allow(missing_docs)]
    Finish {
        id: String,
        /// Poll the branch run in-line instead: stop on a red run, hand off on
        /// green or at the SUSI_FINISH_WAIT_MAX budget (the old default)
        #[arg(long, conflicts_with = "no_wait")]
        wait: bool,
        /// Hand off at the push (the default; kept for scripts that pass it)
        #[arg(long)]
        no_wait: bool,
    },
    /// Keep the clean primary checkout current with remote main (local process).
    Watch,

    /// Create your own worktree on a fresh branch off origin/main, ready to
    /// work in (hooks installed, primary checkout parked). Prints `cd <path>`.
    Start {
        /// Branch/worktree name (default: <agent>-<yyyymmdd>-<hhmmss>-<pid>)
        name: Option<String>,
        /// The commit to branch from (default: origin/main)
        base: Option<String>,
        /// Who you are (default: $SUSI_AGENT, else the agent tool you run
        /// inside — never your git login)
        #[arg(long)]
        agent: Option<String>,
    },
    /// Give this worktree an identity of its own: <TOOL><WORKTREE-ID> as claim
    /// token and git author, never the shared git login or `PRIMARY`. `check`
    /// and `start` do this for you; run it to fix a borrowed identity by hand.
    /// Reclaim finished agent worktrees that have merged into origin/main
    Prune {
        /// Actually remove the worktrees (default: report only)
        #[arg(long)]
        apply: bool,
        /// Remove finished worktrees of all agents, not just yours
        #[arg(long)]
        all: bool,
    },
    #[command(name = "own-identity")]
    #[allow(missing_docs)]
    OwnIdentity,
    /// Mandates 49/50 at the git hooks (`.githooks/workflow-guard` execs this):
    /// refuse commits in the primary checkout or on main, and a push that
    /// deletes, rewrites or is behind origin/main. `push` reads the refs git
    /// hands the pre-push hook on stdin. Needs git only: no shell or coreutils.
    Guard {
        /// The hook that is running
        #[arg(value_enum)]
        stage: GuardStage,
    },
    /// Fetch branch tips, task claims and close receipts from the remote in
    /// one round trip: the snapshot the push guard and CI's compliance job
    /// judge from (`scripts/fetch-workflow-refs.sh` is CI's copy of it).
    #[command(name = "fetch-refs")]
    FetchRefs {
        /// The remote to fetch from
        #[arg(default_value = "origin")]
        remote: String,
    },
    /// Give a worktree a cargo target of its own, warm from the clone's
    /// snapshot of origin/main (`scripts/seed-target.sh` execs this): a link
    /// to the old shared target is replaced (the link only), a missing or
    /// empty target is copied from the snapshot and stripped to registry and
    /// git crates, a target that holds a build is left alone. Never fails.
    #[command(name = "seed-target")]
    SeedTarget {
        /// Replace a link now and copy in a detached process (the hooks)
        #[arg(long, conflicts_with = "refresh")]
        background: bool,
        /// Rebuild origin/main in a clone of its own and republish the snapshot
        /// (the primary watcher runs this)
        #[arg(long)]
        refresh: bool,
        /// With --refresh: refresh a snapshot younger than SUSI_SEED_REFRESH_SECS too
        #[arg(long, requires = "refresh")]
        force: bool,
        /// Export this checkout's target as a snapshot at DIR: copied, stripped
        /// to registry and git crates, and marked (the cloud target seed
        /// workflow publishes it; scripts/cloud-target-seed.sh unpacks it)
        #[arg(long, value_name = "DIR", conflicts_with_all = ["refresh", "background", "dir"])]
        export: Option<std::path::PathBuf>,
        /// With --refresh: write the refresh's report and build output here.
        /// Otherwise: append the seeding's report here instead of stderr (the
        /// background copy, which may hold no caller's output)
        #[arg(long, value_name = "FILE")]
        log: Option<std::path::PathBuf>,
        /// The worktree to seed (default: the one this runs in)
        #[arg(conflicts_with = "refresh")]
        dir: Option<std::path::PathBuf>,
    },
    /// Keep the primary checkout parked at origin/main so nobody works in it
    /// (`scripts/park-primary.sh` execs this): on `main`, fast-forwarded;
    /// detached there only when another worktree holds `main`. Refuses, and
    /// says why, when parking would lose uncommitted or unmerged work.
    #[command(name = "park-primary")]
    ParkPrimary {
        /// Only fast-forward an already-parked `main`, silently or not at
        /// all: never switch a branch or touch a dirty tree (every workflow
        /// check and hook runs this)
        #[arg(long)]
        sync_only: bool,
    },
    /// Check that this checkout follows the susi workflow (Mandates 49-51)
    Check {
        /// Machine-readable output
        #[arg(long)]
        json: bool,
        /// Who you are (default: $SUSI_AGENT, your worktree branch, or your git user)
        #[arg(long)]
        agent: Option<String>,
    },
}

/// `workflow-guard <commit|merge|push>`, the hook's argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum GuardStage {
    /// pre-commit
    Commit,
    /// pre-merge-commit
    Merge,
    /// pre-push (the pushed refs on stdin)
    Push,
}
