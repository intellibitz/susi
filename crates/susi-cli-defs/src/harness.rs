//! `clap` definitions for the `harness` command family, moved verbatim from
//! `src/cli/harness_cli.rs` (D-SP-4 Stage 4c). The handlers stay in the root
//! crate, which re-exports these types from `src/cli/harness_cli.rs`.
//!
//! A variant marked `#[allow(missing_docs)]` keeps undocumented fields (or is
//! hidden) on purpose: clap turns every `///` comment into `--help` text, and
//! this move must leave the visible help byte-identical.
use clap::Subcommand;
use std::path::PathBuf;

/// Where `--ledger` points when it is not given, in `susi_harness::fleet::ledger_for`'s
/// order. One text for every command that takes it: the three copies said
/// the workspace came before `SUSI_HARNESS_LEDGER`, the reverse of the code,
/// so a user with the setting looked for spend and refusals in the wrong
/// file.
macro_rules! ledger_default {
    () => {
        "(default: `SUSI_HARNESS_LEDGER` if set, else `out/ledger.jsonl` in a susi workspace, \
         else `harness/ledger.jsonl` in the data home, `~/.susi` unless `SUSI_HOME` is set; \
         the same default standalone `susi-harness` uses)"
    };
}

/// Subcommands of `susi harness`: author a plan, run a mission, serve the harness API.
#[allow(clippy::large_enum_variant)]
// a CLI flag bag: every variant is argument storage, and boxing one would only relocate the flags clap derives in place
#[derive(Debug, Subcommand)]
pub enum HarnessCommands {
    /// Author a plan from a roadmap vector, through the metered gateway
    Author {
        /// Gateway base URL (the harness gateway)
        #[arg(long)]
        gateway: String,
        /// Model id the gateway prices (such as `deepseek-pro`)
        #[arg(long, default_value = "auto")]
        model: String,
        /// Roadmap file
        #[arg(long, default_value = ".agents/roadmap.json")]
        roadmap: PathBuf,
        /// Vector id, e.g. VC-203-001
        vector: String,
        /// Where to write the plan
        #[arg(long, default_value = "plan.json")]
        out: PathBuf,
        /// Upper bound on tasks
        #[arg(long, default_value_t = 3)]
        max_tasks: usize,
        /// Worker models tasks are spread across (comma-separated)
        #[arg(long, default_value = "auto")]
        workers: String,
    },
    /// Plan a batch of dependency-ready queue tasks; each entry claims its id
    #[command(name = "plan")]
    PlanQueue {
        /// The susi target repository
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Where to write the plan
        #[arg(long, default_value = "plan.json")]
        out: PathBuf,
        /// Mission id
        #[arg(long, default_value = "roadmap-batch")]
        mission: String,
        /// Upper bound on tasks in the batch
        #[arg(long, default_value_t = 1)]
        limit: usize,
        /// Worker models, assigned round-robin (comma-separated)
        #[arg(long, default_value = "auto")]
        workers: String,
        /// Only tasks whose roadmap vector is this id
        #[arg(long)]
        roadmap: Option<String>,
        /// Gateway the plan will run through: a listed worker whose provider
        /// has no key set here stays when that gateway answers for it (an
        /// explicit --upstream, or a gateway with its own key)
        #[arg(long)]
        gateway: Option<String>,
    },
    /// Run a plan: prepare in parallel, land in order, persist after each task
    #[allow(missing_docs)]
    Run {
        /// Plan file
        plan: PathBuf,
        /// The susi target repository
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Supervisor resume state
        #[arg(long, default_value = "supervisor-state.json")]
        state: PathBuf,
        /// Gateway base URL (the harness gateway)
        #[arg(long)]
        gateway: String,
        /// Model id the gateway prices
        #[arg(long, default_value = "auto")]
        model: String,
        /// Per-turn deadline, seconds
        #[arg(long, default_value_t = 900)]
        timeout: u64,
        /// Tasks prepared at once (landing is always serial)
        #[arg(long, default_value_t = 1)]
        parallel: usize,
        /// Worktree name prefix
        #[arg(long, default_value = "harness")]
        prefix: String,
        /// Models a task may escalate to when its own model does not pass
        /// (comma-separated)
        #[arg(long, default_value = "auto")]
        workers: String,
        /// Mission cap in dollars
        #[arg(long, default_value = "5")]
        budget_usd: String,
        /// Price catalogue used to rank workers and bound spend
        #[arg(long, default_value = "deploy/harness/prices.json")]
        catalogue: PathBuf,
        /// Daily spend ceiling in dollars, checked against the ledger before a run
        #[arg(long, default_value = "25")]
        daily_usd: String,
        #[arg(long, help = concat!("Ledger the gateway writes (spend authority) ", ledger_default!()))]
        ledger: Option<PathBuf>,
        /// Control log the harness API writes pause/resume/abort to
        #[arg(long, default_value = "out/control.jsonl")]
        controls: PathBuf,
        /// Self-build lifecycle ledger the RSI scorecard reads
        #[arg(long, default_value = ".agents/rsi-loop.jsonl")]
        rsi_log: PathBuf,
        /// Hard cap on attempts per task
        #[arg(long, default_value_t = 3)]
        attempts: usize,
        /// Where the adapter keeps prompts and results
        #[arg(long, default_value = "harness-state")]
        adapter_state: PathBuf,
        /// The susi binary the adapter drives (default: this one)
        #[arg(long)]
        susi: Option<PathBuf>,
        /// Host-signed susi-delegation/v1 envelope JSON (or @file) attenuating this run
        #[arg(long)]
        delegation: Option<String>,
    },
    /// Print the self-build (RSI) scorecard for the mission lifecycle ledger
    Scorecard {
        /// Lifecycle ledger the supervisor writes
        #[arg(long, default_value = ".agents/rsi-loop.jsonl")]
        events: PathBuf,
        /// Limit the report to one task (repeatable)
        #[arg(long)]
        task: Vec<String>,
        /// Machine-readable report
        #[arg(long)]
        json: bool,
        /// Fail unless every task reached every phase
        #[arg(long)]
        require_complete: bool,
        /// Python executable
        #[arg(long, default_value = "python")]
        python: PathBuf,
    },
    /// Serve the harness JSON/SSE API: spend, tasks, workers, warnings and
    /// operator commands
    // `cockpit` is the pre-zero-UI name, kept as a hidden deprecated alias
    // for one release (zero-UI plan A2).
    #[command(alias = "cockpit")]
    Api {
        /// Ledger the gateway writes
        #[arg(long)]
        ledger: PathBuf,
        /// Event feed (default: beside the ledger)
        #[arg(long)]
        events: Option<PathBuf>,
        /// Supervisor resume state to show task progress
        #[arg(long)]
        state: Option<PathBuf>,
        /// Plan the supervisor is running
        #[arg(long)]
        plan: Option<PathBuf>,
        /// Control log the operator's commands are appended to
        #[arg(long)]
        controls: Option<PathBuf>,
        /// Price catalogue, so each worker's rate is shown
        #[arg(long)]
        catalogue: Option<PathBuf>,
        /// Agent-graph bus file (overrides SUSI_HARNESS_GRAPH and the
        /// graph.jsonl beside the ledger)
        #[arg(long)]
        graph: Option<PathBuf>,
        /// Mission cap in dollars
        #[arg(long)]
        budget_usd: Option<String>,
        /// Mission to focus
        #[arg(long)]
        mission: Option<String>,
        /// Interface to bind
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Port to bind
        #[arg(long, default_value_t = 9181)]
        port: u16,
    },
    /// Print the harness API snapshot for a ledger
    Status {
        /// Ledger the gateway wrote
        #[arg(long)]
        ledger: PathBuf,
        /// Event feed (optional)
        #[arg(long)]
        events: Option<PathBuf>,
    },
    /// Print the provider registry, models and aliases as JSON (no network)
    #[command(group(clap::ArgGroup::new("live").args(["verify", "discover"])))]
    Models {
        /// Check each selected provider's live models endpoint for drift
        #[arg(long)]
        verify: bool,
        /// List the chat models each keyed provider serves beyond the
        /// registry, as an overlay with the prices it publishes
        #[arg(long)]
        discover: bool,
        /// Limit the check to one provider id (requires --verify or --discover)
        #[arg(long, requires = "live")]
        provider: Option<String>,
        /// Filter for models with valid API keys present and sort by budget cost
        #[arg(long)]
        available: bool,
    },
    /// Print the one-line budget report and any top-up alert, without running
    #[allow(missing_docs)]
    Budget {
        #[arg(long, help = concat!("Ledger the spend is read from ", ledger_default!()))]
        ledger: Option<PathBuf>,
    },
    /// Serve the metered multi-provider gateway directly from susi
    #[allow(missing_docs)]
    Gateway {
        /// Port to bind (default: 9180)
        #[arg(long, default_value_t = 9180)]
        port: u16,
        /// Interface to bind (default: 127.0.0.1). Any address other machines
        /// can reach needs --accounts with at least one account
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Price catalogue
        #[arg(long, default_value = "deploy/harness/prices.json")]
        catalogue: PathBuf,
        #[arg(long, help = concat!("Ledger path ", ledger_default!()))]
        ledger: Option<PathBuf>,
        /// Upstream URL or "direct" for multi-provider routing
        #[arg(long, default_value = "direct")]
        upstream: String,
        /// Upstream call timeout in seconds
        #[arg(long, default_value_t = 120)]
        timeout: u64,
        /// Accounts file: every caller must present a bearer token naming one
        /// of its accounts (optional on loopback, required on any other host)
        #[arg(long)]
        accounts: Option<PathBuf>,
    },
}
