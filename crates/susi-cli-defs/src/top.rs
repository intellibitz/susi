//! `clap` definitions for the `top` command family, moved verbatim from
//! `src/cli/top_cli.rs` (D-SP-4 Stage 4d). The handlers stay in the root
//! crate, which re-exports these types from `src/cli/top_cli.rs`.
//!
//! An item marked `#[allow(missing_docs)]` keeps undocumented fields (or is
//! hidden) on purpose: clap turns every `///` comment into `--help` text, and
//! this move must leave the visible help byte-identical.
use clap::Args;

#[derive(Args, Debug, Clone)]
#[allow(missing_docs)] // clap uses doc comments as user-facing --help; undocumented fields stay undocumented to preserve the CLI help snapshot.
pub struct TopArgs {
    /// Output the operator control surface state as JSON
    #[arg(long)]
    pub json: bool,

    /// Filter to a specific state dimension: workload, resources, providers, config, federation, evidence
    #[arg(long)]
    pub filter: Option<String>,
}
