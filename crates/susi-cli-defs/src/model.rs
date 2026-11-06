//! `clap` definitions for the `model` command family, moved verbatim from
//! `src/cli/model_cli.rs` (D-SP-4 Stage 4d). The handlers stay in the root
//! crate, which re-exports these types from `src/cli/model_cli.rs`.
//!
//! An item marked `#[allow(missing_docs)]` keeps undocumented fields (or is
//! hidden) on purpose: clap turns every `///` comment into `--help` text, and
//! this move must leave the visible help byte-identical.
use clap::Subcommand;

/// Subcommands of `susi model`: deterministic coding/agent model control plane.
#[derive(Debug, Subcommand)]
pub enum ModelCommands {
    /// Show the top developer/agent-focused models and readiness
    List,
    /// Check credentials/endpoints (does not call a paid model)
    #[allow(missing_docs)]
    Doctor { model: Option<String> },
    /// Show setup instructions and the effective model wiring
    #[allow(missing_docs)]
    Setup { model: String },
    /// Override engine/model/protocol/api_key_env via JSON file
    #[allow(missing_docs)]
    Configure {
        model: String,
        file: std::path::PathBuf,
    },
    /// Remove a host override and restore the bundled coding-model entry
    #[allow(missing_docs)]
    Reset { model: String },
    /// Prefer a coding model for subsequent routing (`selected_model_override`)
    #[allow(missing_docs)]
    Prefer { model: String },
    /// Paid probe: send a short completion to verify auth and model id
    #[allow(missing_docs)]
    Probe {
        model: String,
        #[arg(default_value = "Reply with exactly: ok")]
        prompt: String,
    },
}
