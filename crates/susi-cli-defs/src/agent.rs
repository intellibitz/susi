//! `clap` definitions for the `agent` command family, moved verbatim from
//! `src/cli/agent_cli.rs` (D-SP-4 Stage 4c). The handlers stay in the root
//! crate, which re-exports these types from `src/cli/agent_cli.rs`.
//!
//! A variant marked `#[allow(missing_docs)]` keeps undocumented fields (or is
//! hidden) on purpose: clap turns every `///` comment into `--help` text, and
//! this move must leave the visible help byte-identical.
use clap::Subcommand;
use std::path::PathBuf;

/// Subcommands of `susi agents`: manage external task executors end to end.
#[derive(Debug, Subcommand)]
pub enum AgentCommands {
    /// Show curated executors, native adapters, and installation documentation
    List,
    /// Check local prerequisites (does not invoke a paid model)
    #[allow(missing_docs)]
    Doctor { agent: Option<String> },
    /// Show setup instructions and the effective adapter configuration
    #[allow(missing_docs)]
    Setup { agent: String },
    /// Load an adapter JSON file into trusted host configuration
    #[allow(missing_docs)]
    Configure { agent: String, file: PathBuf },
    /// Remove a host override and restore the bundled adapter
    #[allow(missing_docs)]
    Reset { agent: String },
    /// Start a task; defaults to a detached worker with durable output
    #[allow(missing_docs)]
    Run {
        agent: String,
        /// Wait for the task to finish in this process
        #[arg(long)]
        wait: bool,
        /// Task prompt, passed as a single argv value, never evaluated by a shell
        prompt: String,
    },
    /// List durable tasks in the current workspace
    Tasks,
    /// Inspect a task; --refresh queries a cloud provider after worker loss/completion
    #[allow(missing_docs)]
    Status {
        task_id: String,
        #[arg(long)]
        refresh: bool,
    },
    /// Read the end of the task's captured output
    #[allow(missing_docs)]
    Logs {
        task_id: String,
        #[arg(long)]
        stderr: bool,
        #[arg(long, default_value_t = 65536)]
        bytes: u64,
    },
    /// Request cancellation; cloud tasks are stopped through their native API
    #[allow(missing_docs)]
    Cancel { task_id: String },
    /// Send a follow-up to an existing cloud task; then use status --refresh
    #[allow(missing_docs)]
    Send { task_id: String, message: String },
    /// Run a new task with the original prompt (does not resume the vendor session)
    #[allow(missing_docs)]
    Retry {
        task_id: String,
        #[arg(long)]
        wait: bool,
    },
    #[command(hide = true)]
    #[allow(missing_docs)]
    Worker { task_id: String },
}
