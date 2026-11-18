//! `clap` definitions for the `run` command family, moved verbatim from
//! `src/cli/run_cli.rs` (D-SP-4 Stage 4d). The handlers stay in the root
//! crate, which re-exports these types from `src/cli/run_cli.rs`.
//!
//! An item marked `#[allow(missing_docs)]` keeps undocumented fields (or is
//! hidden) on purpose: clap turns every `///` comment into `--help` text, and
//! this move must leave the visible help byte-identical.
use clap::Args;

/// Arguments for `susi run`.
#[derive(Args, Debug, Clone)]
pub struct RunArgs {
    /// Model name (e.g. `qwen2.5-coder:7b`, `claude-3-7-sonnet`, `deepseek-chat`)
    #[arg(short = 'm', long = "model")]
    pub model: Option<String>,

    /// System prompt / instructions
    #[arg(short = 's', long = "system")]
    pub system: Option<String>,

    /// Disable streaming output (buffers complete response before printing)
    #[arg(long = "no-stream")]
    pub no_stream: bool,

    /// Output JSON with metadata (duration, token throughput, serving backend)
    #[arg(long = "json")]
    pub json: bool,

    /// Positional model name and/or prompt tokens
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub prompt: Vec<String>,
}
