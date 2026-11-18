//! `clap` definitions for the `intent` command family, moved verbatim from
//! `src/cli/intent_cli.rs` (D-SP-4 Stage 4c). The handlers stay in the root
//! crate, which re-exports these types from `src/cli/intent_cli.rs`.
//!
//! A variant marked `#[allow(missing_docs)]` keeps undocumented fields (or is
//! hidden) on purpose: clap turns every `///` comment into `--help` text, and
//! this move must leave the visible help byte-identical.
use clap::Subcommand;

/// Subcommands of `susi intent`: the semantic intent bus.
#[derive(Debug, Subcommand)]
pub enum IntentCommands {
    /// Advertise a provider capability (NL intent).
    #[allow(missing_docs)]
    Advertise {
        #[arg(short, long)]
        from: String,
        #[arg(short, long)]
        intent: String,
    },
    /// Publish a need and print matched providers.
    #[allow(missing_docs)]
    Need {
        #[arg(short, long)]
        from: String,
        #[arg(short, long)]
        intent: String,
        #[arg(short, long, default_value_t = 0.2)]
        min_score: f32,
        #[arg(short, long, default_value_t = 5)]
        limit: usize,
    },
    /// List active providers and needs.
    List,
}
