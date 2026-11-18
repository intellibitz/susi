//! `clap` definitions for the `brain` command family, moved verbatim from
//! `src/cli/brain_cli.rs` (D-SP-4 Stage 4c). The handlers stay in the root
//! crate, which re-exports these types from `src/cli/brain_cli.rs`.
//!
//! A variant marked `#[allow(missing_docs)]` keeps undocumented fields (or is
//! hidden) on purpose: clap turns every `///` comment into `--help` text, and
//! this move must leave the visible help byte-identical.
use clap::Subcommand;
use std::path::PathBuf;

/// Subcommands of `susi brain`: which model susi trusts for which kind of work.
#[derive(Debug, Subcommand)]
pub enum BrainCommands {
    /// Show the evidence-ranked providers for every task class (default)
    Status {
        /// Number of top ranked providers to show per class
        #[arg(long, default_value_t = 5)]
        top: usize,
        /// Filter by specific task class (e.g. reflex, chat, code, reasoning, estate_ops)
        #[arg(long)]
        class: Option<String>,
        /// Output raw JSON instead of human summary table
        #[arg(long)]
        json: bool,
    },
    /// Set shared cloud spending and request limits
    #[allow(missing_docs)]
    Budget {
        #[arg(long, conflicts_with = "allow_paid")]
        free_only: bool,
        /// Authorize paid inference within the configured limits
        #[arg(long)]
        allow_paid: bool,
        /// Budget dial setting: low, balanced, or max
        #[arg(long)]
        level: Option<String>,
        #[arg(long)]
        max_spend_microusd: Option<u64>,
        #[arg(long)]
        max_tokens: Option<u64>,
        #[arg(long)]
        max_requests: Option<u64>,
        #[arg(long)]
        max_parallel: Option<u64>,
        /// Explicitly start a new accounting window; unresolved calls forbid reset
        #[arg(long)]
        new_window: bool,
    },
    /// Show which task class a prompt is routed as
    #[allow(missing_docs)]
    Classify {
        #[arg(trailing_var_arg = true)]
        prompt: Vec<String>,
    },
    /// Explain the brain's routing decision for a prompt
    #[allow(missing_docs)]
    Explain {
        #[arg(trailing_var_arg = true)]
        prompt: Vec<String>,
    },
    /// Export brain evidence bundle to portable JSON format
    Export {
        /// Output file path (defaults to stdout if omitted)
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Import and merge brain evidence bundle from a file
    Import {
        /// Path to bundle file
        file: PathBuf,
    },
    /// Show the measured capability matrix: every model's success rate,
    /// latency and cost per verified outcome at each task class
    Matrix,
    /// Show the 3D Pareto frontier, Wilson 95% lower confidence bounds, and
    /// Bayesian UCB1 exploration-exploitation indices across providers
    Pareto {
        /// Filter by specific task class (e.g. reflex, chat, code, reasoning, estate_ops)
        #[arg(long)]
        class: Option<String>,
    },
    /// Show the measured prompt-cache hit rate and the estimated saving
    /// cache-first context assembly has realised so far
    Cache,
    /// Forget all recorded outcomes (new account, changed model lineup)
    Reset,
    /// Run the fixed prompt set against a fake or configured engine; record evidence
    Bench {
        /// Write JSONL evidence under this path (default: ~/.susi/brain_bench.jsonl)
        #[arg(long)]
        out: Option<std::path::PathBuf>,
    },
    /// Probe a registered provider with the fixed scout benchmark and record
    /// the verified outcomes as brain evidence
    Probe {
        /// Registered provider name (see `susi brain` ranking output)
        provider: String,
        /// Do not record the outcomes into the brain evidence store
        #[arg(long)]
        no_record: bool,
    },
}

impl Default for BrainCommands {
    fn default() -> Self {
        Self::Status {
            top: 5,
            class: None,
            json: false,
        }
    }
}
