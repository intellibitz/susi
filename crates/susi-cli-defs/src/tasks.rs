//! `clap` definitions for the `tasks` command family, moved verbatim from
//! `src/cli/tasks_cli.rs` (D-SP-4 Stage 4d). The handlers stay in the root
//! crate, which re-exports these types from `src/cli/tasks_cli.rs`.
//!
//! An item marked `#[allow(missing_docs)]` keeps undocumented fields (or is
//! hidden) on purpose: clap turns every `///` comment into `--help` text, and
//! this move must leave the visible help byte-identical.
use clap::Subcommand;
use std::path::PathBuf;

/// Subcommands of `susi tasks`: the shared task queue.
#[derive(Debug, Subcommand)]
pub enum TaskCommands {
    /// List open tasks with who holds each claim (default)
    List {
        /// Also show closed tasks
        #[arg(long)]
        done: bool,
        /// Emit machine-readable JSON (this is already the default; accepted so
        /// scripts can be explicit, matching `susi tasks audit --json`)
        #[arg(long)]
        json: bool,
    },
    /// Add a task; it needs an acceptance command that decides "done"
    #[allow(missing_docs)]
    Add {
        title: String,
        #[arg(long, default_value = "")]
        goal: String,
        /// s | m | l
        #[arg(long, default_value = "m")]
        size: String,
        /// Task ids that must close first (repeatable)
        #[arg(long = "dep")]
        deps: Vec<String>,
        /// Acceptance command, split on spaces, e.g. "cargo test -p susi-gemi brain"
        #[arg(long)]
        accept: String,
        /// Roadmap vector this delivers (VC-<n>-<n>, from .agents/roadmap.json)
        #[arg(long)]
        roadmap: Option<String>,
        /// Who is adding it (default: $SUSI_AGENT, your worktree branch, or your git user)
        #[arg(long)]
        agent: Option<String>,
    },
    /// Author backlog artifacts from a spec file: vectors into
    /// .agents/roadmap.json and tasks into .agents/tasks/, both through the
    /// same gates a human's edits meet. The spec is
    /// {"rationale","vectors":[{"id","priority","vector","mastery_target","type","depends_on"}],
    ///  "tasks":[{"title","goal","size","deps","accept","roadmap"}]}. The path is
    /// rate-bound: an agent may hold AUTHORED_OPEN_MAX open authored tasks.
    /// With --unqueued and no file, susi's own coverage audit drafts a
    /// "Verify mastery" task for every unqueued vector through the same path.
    /// With --from-failures and no file, a mission that did not succeed
    /// drafts its own repair task, deduplicated by failure kind.
    #[allow(missing_docs)]
    Author {
        /// Spec JSON file (omit with --unqueued / --from-failures)
        file: Option<PathBuf>,
        /// Author verification tasks for every unqueued roadmap vector
        #[arg(long)]
        unqueued: bool,
        /// Author repair tasks from recorded mission failures
        #[arg(long)]
        from_failures: bool,
        #[arg(long)]
        agent: Option<String>,
    },
    /// List open machine-authored tasks — the review surface for the author path
    Authored,
    /// Load the versioned RSI corpus, judge one candidate run against its
    /// held-out suite, and durably record the contamination/promotion
    /// evidence (VC-201-001). `--input` is a JSON file shaped
    /// {"candidate":{"id","actual_output","expected_results"},
    ///  "candidate_can_write","training_access","memory_access"}.
    #[command(name = "eval-corpus")]
    EvalCorpus {
        /// Path to the corpus JSON document
        #[arg(long, value_name = "FILE")]
        corpus: PathBuf,
        /// Path to the JSON-encoded eval run (candidate + access sets)
        #[arg(long, value_name = "FILE")]
        input: PathBuf,
    },
    /// Roadmap coverage and mastery: which vectors have tasks, and which of
    /// them claim delivery in their own narrative
    Roadmap {
        /// Only vectors no task is linked to
        #[arg(long)]
        uncovered: bool,
        /// Only vectors that do not claim delivery and have nothing queued
        #[arg(long)]
        unqueued: bool,
    },
    /// Claim a task (atomic; fails if someone else holds a live claim)
    #[allow(missing_docs)]
    Claim {
        id: String,
        /// Reserve a repo-relative file/directory (repeatable); overlapping claims fail.
        #[arg(long = "scope")]
        scopes: Vec<String>,
        /// Claim with no reservation at all (deliberate: the commit hook and CI
        /// then refuse any code under it)
        #[arg(long)]
        unscoped: bool,
        #[arg(long)]
        agent: Option<String>,
        /// Lease length; an expired claim can be taken over. Defaults to 4h, or
        /// 12h for a size-l task, which will not finish inside a shorter one.
        #[arg(long)]
        hours: Option<u64>,
    },
    /// Renew an owned live task lease, retaining its scopes.
    #[allow(missing_docs)]
    Renew {
        id: String,
        #[arg(long)]
        agent: Option<String>,
        /// Lease length. Defaults to the claim's own: 4h, or 12h for a size-l
        /// task.
        #[arg(long)]
        hours: Option<u64>,
    },
    /// Give up a claim
    #[allow(missing_docs)]
    Release {
        id: String,
        #[arg(long)]
        agent: Option<String>,
        /// Release a claim held by someone else
        #[arg(long)]
        force: bool,
        /// Give up an accepted task whose work is not merged yet, recording why.
        /// Without it a close that is not on origin/main is held, not dropped.
        #[arg(long)]
        abandon: Option<String>,
    },
    /// Run the acceptance check and, only if it passes, close the task
    #[allow(missing_docs)]
    Close {
        id: String,
        #[arg(long)]
        agent: Option<String>,
    },
    /// Reconcile what agents accepted against what actually reached origin/main
    Audit {
        /// Machine-readable report — the per-agent ranking input (Mandate 56)
        #[arg(long)]
        json: bool,
        /// Exit non-zero when any accepted task is still unpublished
        #[arg(long)]
        strict: bool,
    },
    /// Re-run the acceptance of merged tasks against this tree (CI, main only)
    #[command(hide = true)]
    VerifyMerged {
        /// How many unverified merges one run may check
        #[arg(long, default_value_t = 5)]
        limit: usize,
        /// Wall-clock budget for the whole sweep, seconds; what does not fit is
        /// deferred to the next run rather than holding the job
        #[arg(long, default_value_t = crate::VERIFY_BUDGET_SECS)]
        budget_secs: u64,
    },
}
