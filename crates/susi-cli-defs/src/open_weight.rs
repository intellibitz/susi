//! `clap` definitions for the `open_weight` command family, moved verbatim from
//! `src/cli/open_weight_cli.rs` (D-SP-4 Stage 4d). The handlers stay in the root
//! crate, which re-exports these types from `src/cli/open_weight_cli.rs`.
//!
//! An item marked `#[allow(missing_docs)]` keeps undocumented fields (or is
//! hidden) on purpose: clap turns every `///` comment into `--help` text, and
//! this move must leave the visible help byte-identical.
use clap::Subcommand;

/// Subcommands of `susi open-weight`: open-weight frontier model control plane.
#[derive(Debug, Subcommand)]
pub enum OpenWeightCommands {
    /// Show the top open-weight frontier models and readiness
    List,
    /// Check local endpoint / pull status (does not download weights)
    #[allow(missing_docs)]
    Doctor { model: Option<String> },
    /// Show setup instructions and effective wiring
    #[allow(missing_docs)]
    Setup { model: String },
    /// Pull weights via `ollama pull <tag>`
    #[allow(missing_docs)]
    Pull { model: String },
    /// Override engine/ollama_tag/protocol via JSON file
    #[allow(missing_docs)]
    Configure {
        model: String,
        file: std::path::PathBuf,
    },
    /// Remove a host override and restore the bundled entry
    #[allow(missing_docs)]
    Reset { model: String },
    /// Prefer an open-weight model for local routing
    #[allow(missing_docs)]
    Prefer {
        model: Option<String>,
        /// Clear the preferred open-weight pin
        #[arg(long)]
        clear: bool,
    },
    /// Local probe: short completion via Ollama/vLLM
    #[allow(missing_docs)]
    Probe {
        model: String,
        #[arg(default_value = "Reply with exactly: ok")]
        prompt: String,
    },
}
