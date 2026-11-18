//! `clap` definitions for the `context_graph` command family, moved verbatim from
//! `src/cli/context_graph_cli.rs` (D-SP-4 Stage 4c). The handlers stay in the root
//! crate, which re-exports these types from `src/cli/context_graph_cli.rs`.
use clap::Subcommand;

/// Subcommands of `susi context-graph`: inspect and query the universal context graph.
#[derive(Debug, Subcommand)]
pub enum ContextGraphCommands {
    /// Show the most recent context-graph events for this workspace.
    Show {
        /// Maximum events to display.
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
    },
    /// Show nodes/edges related to a specific node id.
    Related {
        /// Node id to expand from.
        #[arg(short, long)]
        node: String,
        /// Expansion depth.
        #[arg(short, long, default_value_t = 2)]
        depth: usize,
    },
    /// Summary statistics for the loaded graph.
    Stats,
    /// Compact the append-only context-graph log.
    Compact,
    /// Ingest external context events from a JSONL file.
    Ingest {
        /// Path to a JSONL file where each line is {"source":"...","label":"...","payload":{...}}.
        file: std::path::PathBuf,
        /// Optional workspace to associate with every ingested event.
        #[arg(short, long)]
        workspace: Option<std::path::PathBuf>,
    },
    /// Sample OS-level context (active window, process list) and record it.
    #[command(name = "sample-os")]
    SampleOs {
        /// Optional user name to associate with the sample.
        #[arg(short, long)]
        user: Option<String>,
    },
}
