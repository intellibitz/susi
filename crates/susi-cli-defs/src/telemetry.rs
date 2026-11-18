//! `clap` definitions for the `telemetry` command family, moved verbatim from
//! `src/cli/telemetry_cli.rs` (D-SP-4 Stage 4d). The handlers stay in the root
//! crate, which re-exports these types from `src/cli/telemetry_cli.rs`.
//!
//! An item marked `#[allow(missing_docs)]` keeps undocumented fields (or is
//! hidden) on purpose: clap turns every `///` comment into `--help` text, and
//! this move must leave the visible help byte-identical.
use clap::Subcommand;

/// Subcommands of `susi telemetry`: host telemetry sampling.
#[derive(Debug, Subcommand)]
pub enum TelemetryCommands {
    /// Sample thermal/battery/load telemetry now.
    Sample,
}
