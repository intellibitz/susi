//! `clap` definitions for the `commits` command family, moved verbatim from
//! `src/cli/commits_cli.rs` (D-SP-4 Stage 4c). The handlers stay in the root
//! crate, which re-exports these types from `src/cli/commits_cli.rs`.
use clap::Subcommand;

/// Subcommands of `susi commits`: audit view of the replicated quorum-commit ledger.
#[derive(Debug, Subcommand)]
pub enum CommitsCommands {
    /// List commit records, newest first (default)
    List {
        /// Maximum records to show
        #[arg(short = 'n', long, default_value = "20")]
        limit: usize,
        /// Only records from this coordinator
        #[arg(long)]
        coordinator: Option<String>,
        /// Only records sealed under this consensus term
        #[arg(long)]
        term: Option<u64>,
        /// Only records of this kind (decision, member_add,
        /// member_remove, member_unban)
        #[arg(long)]
        kind: Option<String>,
        /// Emit machine-readable JSON instead of the table — the form
        /// agents and scripts should consume
        #[arg(long)]
        json: bool,
    },
    /// Print the full record (including committed value) for an epoch prefix
    Show {
        /// Epoch hex prefix, e.g. the first 12 chars shown by `susi commits`
        epoch: String,
        /// Emit machine-readable JSON — verification results ride inside
        /// the object instead of trailing human lines
        #[arg(long)]
        json: bool,
    },
    /// Whole-ledger consistency check: signature failures, sequence gaps,
    /// equivocation, and commits from non-elected coordinators
    Audit {
        /// Exit nonzero when anomalies are found — lets CI and operators
        /// gate on ledger health instead of eyeballing output
        #[arg(long)]
        strict: bool,
        /// Emit machine-readable JSON — agents and CI get structured
        /// anomalies instead of parsing log lines
        #[arg(long)]
        json: bool,
    },
    /// Fold the ledger into the cluster's consensus view (term, leader,
    /// per-coordinator high-water marks) — the state machine is a pure
    /// function of the log
    Replay {
        /// Emit machine-readable JSON — the consensus view as data
        #[arg(long)]
        json: bool,
    },
    /// Proactively pull records this node is missing from verified peers —
    /// the receive-path repair only fires when a *push* arrives, so a node
    /// that was offline during replication needs this to catch up
    Sync,
    /// Fold the ledger into a snapshot and move pre-snapshot records to
    /// commit_log.archive.jsonl — bounds the live file's growth (Raft's
    /// InstallSnapshot analog). Local storage management, not a
    /// consensus event: the compacted and uncompacted views of the same
    /// history are identical
    Compact,
}
