//! `clap` definitions for the `ambient` command family, moved verbatim from
//! `src/cli/ambient_cli.rs` (D-SP-4 Stage 4c). The handlers stay in the root
//! crate, which re-exports these types from `src/cli/ambient_cli.rs`.
use clap::Subcommand;

/// Subcommands of `susi ambient`: ambient workspace to context graph and vector index sync.
#[derive(Debug, Subcommand)]
pub enum AmbientCommands {
    /// Scan workspace once, record changes, refresh semantic index.
    Pulse,
    /// Start background ambient indexer for this workspace (process-local).
    Start,
}
