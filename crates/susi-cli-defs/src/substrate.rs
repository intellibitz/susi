//! `clap` definitions for the `substrate` command family, moved verbatim from
//! `src/cli/substrate_cli.rs` (D-SP-4 Stage 4d). The handlers stay in the root
//! crate, which re-exports these types from `src/cli/substrate_cli.rs`.
//!
//! An item marked `#[allow(missing_docs)]` keeps undocumented fields (or is
//! hidden) on purpose: clap turns every `///` comment into `--help` text, and
//! this move must leave the visible help byte-identical.
use clap::Subcommand;

/// Subcommands of `susi substrate`: Tier S substrate status.
#[derive(Debug, Subcommand)]
pub enum SubstrateCommands {
    /// Show Pluggable / Sandbox / Governance / Host-contract / Reflexes status (default)
    Status,
}
