//! `clap` definitions for the `services` command family, moved verbatim from
//! `src/cli/services_cli.rs` (D-SP-4 Stage 4d). The handlers stay in the root
//! crate, which re-exports these types from `src/cli/services_cli.rs`.
//!
//! An item marked `#[allow(missing_docs)]` keeps undocumented fields (or is
//! hidden) on purpose: clap turns every `///` comment into `--help` text, and
//! this move must leave the visible help byte-identical.
use clap::Subcommand;

/// Subcommands of `susi services`: the leaf-service process table.
#[derive(Debug, Subcommand)]
pub enum ServicesCommands {
    /// Show every leaf service, its supervised pid, and live health (default)
    Status {
        /// Emit machine-readable JSON — agent callers get the process
        /// table as data instead of parsing the table view
        #[arg(long)]
        json: bool,
    },
    /// Terminate a supervised service; the daemon respawns it on its next probe
    Restart {
        /// Service name: susi-paths, susi-error, susi-config, susi-sandbox, susi-native
        name: String,
    },
    /// Stop a supervised service and hold it down — the supervisor will
    /// not respawn it until `susi services start` clears the stop flag.
    /// Distinct from restart: the service stays dead by operator intent.
    Stop {
        /// Service name: susi-paths, susi-error, susi-config, susi-sandbox, susi-native
        name: String,
    },
    /// Clear an operator stop and let the supervisor bring the service up
    Start {
        /// Service name: susi-paths, susi-error, susi-config, susi-sandbox, susi-native
        name: String,
    },
    /// Tail a supervised service's log file (the daemon redirects each
    /// spawned service's stderr to `substrate_home/logs/<name>.log`)
    Logs {
        /// Service name: susi-paths, susi-error, susi-config, susi-sandbox, susi-native
        name: String,
        /// Number of trailing lines to print
        #[arg(short = 'n', long, default_value = "50")]
        lines: usize,
    },
}
