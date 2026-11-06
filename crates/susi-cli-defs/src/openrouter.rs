//! `clap` definitions for the `openrouter` command family, moved verbatim from
//! `src/cli/openrouter_cli.rs` (D-SP-4 Stage 4d). The handlers stay in the root
//! crate, which re-exports these types from `src/cli/openrouter_cli.rs`.
//!
//! An item marked `#[allow(missing_docs)]` keeps undocumented fields (or is
//! hidden) on purpose: clap turns every `///` comment into `--help` text, and
//! this move must leave the visible help byte-identical.
use clap::Subcommand;

/// Subcommands of `susi openrouter`: OpenRouter control plane.
#[derive(Debug, Subcommand)]
pub enum OpenRouterCommands {
    /// Show OpenRouter status and curated routes
    List,
    /// Check key + endpoint readiness (does not call a paid model unless --live)
    Doctor {
        /// Also hit GET /models to verify the key works
        #[arg(long)]
        live: bool,
    },
    /// Show setup instructions and effective wiring
    Setup,
    /// Prefer OpenRouter for cloud routing; optionally pin a model/route id
    Prefer {
        /// Catalog id (e.g. claude-sonnet) or raw OpenRouter model slug
        model: Option<String>,
        /// Clear the pinned model (keep cloud preference)
        #[arg(long)]
        clear_model: bool,
    },
    /// Paid probe: short completion via the effective OpenRouter model
    #[allow(missing_docs)]
    Probe {
        #[arg(default_value = "Reply with exactly: ok")]
        prompt: String,
    },
    /// List curated routes, or live provider model ids with --live
    #[allow(missing_docs)]
    Models {
        #[arg(long)]
        live: bool,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
}
