//! `clap` definitions for the `tx` command family, moved verbatim from
//! `src/cli/tx_cli.rs` (D-SP-4 Stage 4d). The handlers stay in the root
//! crate, which re-exports these types from `src/cli/tx_cli.rs`.
//!
//! An item marked `#[allow(missing_docs)]` keeps undocumented fields (or is
//! hidden) on purpose: clap turns every `///` comment into `--help` text, and
//! this move must leave the visible help byte-identical.
use clap::Subcommand;

/// Subcommands of `susi tx`: multi-agent transactions.
#[derive(Debug, Subcommand)]
pub enum TxCommands {
    /// Begin a transaction snapshotting the given files (comma-separated).
    #[allow(missing_docs)]
    Begin {
        #[arg(short, long)]
        description: String,
        #[arg(short, long, default_value = "")]
        files: String,
    },
    /// Commit an open transaction.
    #[allow(missing_docs)]
    Commit {
        #[arg(short, long)]
        id: String,
    },
    /// Abort and restore snapshotted files.
    #[allow(missing_docs)]
    Abort {
        #[arg(short, long)]
        id: String,
    },
    /// List open transactions.
    List,
}
