//! `clap` definitions for the `frontier` command family, moved verbatim from
//! `src/cli/frontier_cli.rs` (D-SP-4 Stage 4c). The handlers stay in the root
//! crate, which re-exports these types from `src/cli/frontier_cli.rs`.
//!
//! A variant marked `#[allow(missing_docs)]` keeps undocumented fields (or is
//! hidden) on purpose: clap turns every `///` comment into `--help` text, and
//! this move must leave the visible help byte-identical.
use clap::Subcommand;

/// Subcommands of `susi frontier`: the top frontier model control plane.
#[derive(Debug, Subcommand)]
pub enum FrontierCommands {
    /// Show the top frontier models and readiness
    List,
    /// Check credentials/endpoints (does not call a paid model)
    #[allow(missing_docs)]
    Doctor { model: Option<String> },
    /// Show setup instructions and effective wiring
    #[allow(missing_docs)]
    Setup { model: String },
    /// Override engine/model/protocol/api_key_env via JSON file
    #[allow(missing_docs)]
    Configure {
        model: String,
        file: std::path::PathBuf,
    },
    /// Remove a host override and restore the bundled entry
    #[allow(missing_docs)]
    Reset { model: String },
    /// Prefer a frontier model for subsequent routing
    #[allow(missing_docs)]
    Prefer {
        model: Option<String>,
        /// Clear the preferred frontier pin
        #[arg(long)]
        clear: bool,
    },
    /// Probe: short completion via cloud or local endpoint
    #[allow(missing_docs)]
    Probe {
        model: String,
        #[arg(default_value = "Reply with exactly: ok")]
        prompt: String,
    },
}
