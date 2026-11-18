//! `clap` definitions for `susi board`: Susi Native Board collaboration and
//! isolated-worktree guard commands.
use clap::Subcommand;

/// Subcommands of `susi board`: collaborate on the Susi Native Board.
#[derive(Debug, Subcommand)]
pub enum BoardCommands {
    /// List collaborative items on the Susi Native Board
    List {
        /// Maximum newest items to print
        #[arg(long)]
        limit: Option<usize>,
        /// Filter items by kind (claim, landed, stop, lift, request, answer, ...)
        #[arg(long)]
        kind: Option<String>,
        /// Print machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// Show the full text of one board item by number
    Show {
        /// Item number (`1..`)
        n: u64,
        /// Print machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// Check the board for open stops, unanswered requests, and integrity findings
    Check {
        /// Acting agent name (defaults to SUSI_AGENT or git config susi.agent)
        #[arg(long)]
        agent: Option<String>,
        /// Permit an empty board
        #[arg(long)]
        allow_empty: bool,
        /// Print machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// Post a collaborative coordination item to the Susi Native Board
    Post {
        /// Item kind (claim, landed, cut, heads-up, request, answer, handoff, decision, blocked, stop, lift, note)
        #[arg(long)]
        kind: String,
        /// Single-line title (at most 200 characters)
        #[arg(long)]
        title: String,
        /// Item body (must contain no credentials)
        #[arg(long, default_value = "")]
        body: String,
        /// Acting agent name (defaults to SUSI_AGENT or git config susi.agent)
        #[arg(long)]
        agent: Option<String>,
        /// Addressee agent for request/handoff/stop items
        #[arg(long)]
        to: Option<String>,
        /// Referenced item number (required for answer and lift)
        #[arg(long)]
        re: Option<u64>,
        /// Task identifier (e.g. T-ANTIGRAVITY-9)
        #[arg(long)]
        task: Option<String>,
        /// Initial item number when starting an empty board
        #[arg(long)]
        start: Option<u64>,
        /// Print machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// Verify isolated worktree ownership and clean collaborative board state
    Guard {
        /// Hook/lifecycle stage (`commit`, `merge`, `push`)
        stage: String,
        /// Acting agent name (defaults to SUSI_AGENT or git config susi.agent)
        #[arg(long)]
        agent: Option<String>,
    },
}
