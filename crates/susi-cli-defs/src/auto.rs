//! `clap` definitions for the `auto` command family, moved verbatim from
//! `src/cli/auto_cli.rs` (D-SP-4 Stage 4c). The handlers stay in the root
//! crate, which re-exports these types from `src/cli/auto_cli.rs`.
use clap::Subcommand;

/// Subcommands of `susi auto`: zero-config auto substrate readiness.
#[derive(Debug, Subcommand)]
pub enum AutoCommands {
    /// Show zero-config auto substrate readiness (default). A snapshot of
    /// what the last prime wrote: the full report is refreshed only by
    /// `auto prime` and commands that prime every catalog (missions,
    /// in-process `mcp call`); the daemon and other commands prime only
    /// what they read, and the daemon never probes agent frameworks.
    Status,
    /// Re-run auto-prime now (models/installed peers; never enables MCP)
    Prime,
}
