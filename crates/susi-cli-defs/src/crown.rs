//! `clap` definitions for the `crown` command family, moved verbatim from
//! `src/cli/crown_cli.rs` (D-SP-4 Stage 4c). The handlers stay in the root
//! crate, which re-exports these types from `src/cli/crown_cli.rs`.
use clap::Subcommand;

/// Subcommands of `susi crown`: verify every Tier S USP holds in source and runtime.
#[derive(Debug, Subcommand)]
pub enum CrownCommands {
    /// Verify all Tier S USPs (default). Exit non-zero if any critical USP fails.
    Verify,
    /// Same as verify but always exit 0 (report only)
    Status,
}
