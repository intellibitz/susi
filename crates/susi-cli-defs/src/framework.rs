//! `clap` definitions for the `framework` command family, moved verbatim from
//! `src/cli/framework_cli.rs` (D-SP-4 Stage 4c). The handlers stay in the root
//! crate, which re-exports these types from `src/cli/framework_cli.rs`.
//!
//! A variant marked `#[allow(missing_docs)]` keeps undocumented fields (or is
//! hidden) on purpose: clap turns every `///` comment into `--help` text, and
//! this move must leave the visible help byte-identical.
use clap::Subcommand;
use std::path::PathBuf;

/// Subcommands of `susi frameworks`: manage agent frameworks and engines end to end.
#[derive(Debug, Subcommand)]
pub enum FrameworkCommands {
    /// Show the ten agent frameworks/engines and setup documentation
    List,
    /// Check local prerequisites (does not invoke a paid model)
    #[allow(missing_docs)]
    Doctor { engine: Option<String> },
    /// Show setup instructions and the effective adapter configuration
    #[allow(missing_docs)]
    Setup { engine: String },
    /// Load an adapter JSON file into trusted host configuration
    #[allow(missing_docs)]
    Configure { engine: String, file: PathBuf },
    /// Remove a host override and restore the bundled adapter
    #[allow(missing_docs)]
    Reset { engine: String },
    /// Start a task; defaults to a detached worker with durable output
    #[allow(missing_docs)]
    Run {
        engine: String,
        #[arg(long)]
        wait: bool,
        prompt: String,
    },
    /// List durable framework tasks in the current workspace
    Tasks,
    #[allow(missing_docs)]
    Status {
        task_id: String,
        #[arg(long)]
        refresh: bool,
    },
    #[allow(missing_docs)]
    Logs {
        task_id: String,
        #[arg(long)]
        stderr: bool,
        #[arg(long, default_value_t = 65536)]
        bytes: u64,
    },
    #[allow(missing_docs)]
    Cancel { task_id: String },
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
