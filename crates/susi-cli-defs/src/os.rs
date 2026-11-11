//! `clap` definitions for the `os` command family, moved verbatim from
//! `src/cli/os_cli.rs` (D-SP-4 Stage 4d). The handlers stay in the root
//! crate, which re-exports these types from `src/cli/os_cli.rs`.
//!
//! An item marked `#[allow(missing_docs)]` keeps undocumented fields (or is
//! hidden) on purpose: clap turns every `///` comment into `--help` text, and
//! this move must leave the visible help byte-identical.
use clap::Subcommand;

/// Subcommands of `susi os`: the operating-system view of the substrate.
#[derive(Debug, clap::Subcommand)]
pub enum OsCommands {
    /// Full substrate status: consensus, services, peers (default)
    Status {
        /// Emit machine-readable JSON instead of the table view
        #[arg(long)]
        json: bool,
    },
    /// Remove stray heavyweight files flagged in `~/.susi/bin` and
    /// report reclaimed space
    Clean,
    /// Discover the local AI ecosystem: hardware, runtimes, models, agents,
    /// frameworks, MCP servers, and running AI processes
    Ecosystem {
        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
        /// Exit unsuccessfully when installed components are unhealthy
        #[arg(long)]
        doctor: bool,
    },
    /// Live runtime view: AI processes by owner (SUSI vs external), managed
    /// models with observed residency, and per-process GPU memory
    Runtime {
        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// Explain the live local-versus-cloud inference placement decision
    Route {
        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
        /// Required model capability, for example `vision`
        #[arg(long)]
        requires: Option<String>,
        /// Maximum acceptable request-cost hint in USD
        #[arg(long)]
        max_cost: Option<f64>,
        /// Prohibit placement on cloud providers
        #[arg(long)]
        no_cloud: bool,
    },
    /// Re-admit a repaired cloud provider after routing quarantine
    RouteReset {
        /// Exact provider id shown by `susi os route --json`
        provider: String,
    },
    /// Unified lifecycle facade for local AI ecosystem components
    #[allow(missing_docs)]
    Manage {
        #[command(subcommand)]
        resource: OsManageCommands,
    },
    /// Explicit cloud/local host provision. The 30s OS tick runs no
    /// operator CLI; probe/list/apply run only on request — that is the OS
    /// contract, not a silent cloud API.
    #[allow(missing_docs)]
    Provision {
        #[command(subcommand)]
        action: OsProvisionAction,
    },
    /// The unified capability bus: discover every A2A/MCP/HTTP/adapter
    /// capability through one typed catalog, dispatch one through its
    /// permission/timeout/cost contract (Mcp tools only today), or issue a
    /// host-signed envelope another component nests its calls under.
    #[allow(missing_docs)]
    Capabilities {
        #[command(subcommand)]
        action: Option<Box<OsCapabilitiesAction>>,
    },
}

/// Actions of `susi os capabilities`: the unified capability bus.
#[derive(Debug, Subcommand)]
pub enum OsCapabilitiesAction {
    /// List every discovered capability (default)
    #[allow(missing_docs)]
    Discover {
        #[arg(long)]
        json: bool,
    },
    /// Dispatch one capability through its permission/timeout/cost contract
    #[allow(missing_docs)]
    Invoke {
        /// Capability id to dispatch (as shown by discover)
        id: String,
        /// Subject credited/audited for the invocation
        #[arg(long, default_value = "operator")]
        subject: String,
        /// Permission held by the subject, e.g. `capability:tool`
        #[arg(long)]
        permission: String,
        /// JSON args passed to the invoked tool
        #[arg(long, default_value = "{}")]
        args: String,
        #[arg(long, default_value_t = 30_000)]
        timeout_ms: u64,
        #[arg(long, default_value_t = u64::MAX)]
        budget_micros: u64,
        /// Host-signed `susi-delegation/v1` envelope (JSON) from the calling
        /// driver: the call nests under it and cannot exceed it
        #[arg(long)]
        delegation: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Issue a host-signed delegation envelope: `subject` may exercise `id`
    /// with `permission`, budget and timeout, nested under `--delegation`
    /// (or a fresh root for `subject`) and never beyond it
    #[allow(missing_docs)]
    Delegate {
        /// Capability (or role) the subject will exercise
        id: String,
        /// Component the envelope is issued to
        #[arg(long, default_value = "operator")]
        subject: String,
        /// Permission the subject may present
        #[arg(long)]
        permission: String,
        #[arg(long, default_value_t = 30_000)]
        timeout_ms: u64,
        #[arg(long, default_value_t = u64::MAX)]
        budget_micros: u64,
        /// Parent host-signed envelope (JSON); absent opens a fresh root
        #[arg(long)]
        delegation: Option<String>,
    },
}

/// Actions of `susi os provision`: explicit cloud/local host provision.
#[derive(Debug, Subcommand)]
pub enum OsProvisionAction {
    /// PATH version-probe of kubectl/docker/aws/gcloud/az (on request only)
    #[allow(missing_docs)]
    Probe {
        #[arg(long)]
        json: bool,
    },
    /// Live inventory (`kubectl get nodes`, `docker ps`, cloud instance list)
    #[allow(missing_docs)]
    List {
        /// kubernetes|docker|aws|gcp|azure
        kind: String,
        #[arg(long)]
        json: bool,
    },
    /// Apply a Kubernetes manifest (`kubectl apply -f`). Other kinds refuse.
    Apply {
        /// kubernetes (kubectl-only)
        kind: String,
        /// Manifest file path
        manifest: std::path::PathBuf,
    },
}

/// Resources of `susi os manage`: the unified lifecycle facade.
#[derive(Debug, Subcommand)]
pub enum OsManageCommands {
    /// Manage a supervised SUSI service
    #[allow(missing_docs)]
    Service {
        #[command(subcommand)]
        action: OsServiceAction,
    },
    /// Manage a local or cloud model definition
    #[allow(missing_docs)]
    Model {
        #[command(subcommand)]
        action: OsModelAction,
    },
    /// Manage an MCP tool server
    #[allow(missing_docs)]
    Mcp {
        #[command(subcommand)]
        action: OsMcpAction,
    },
    /// Manage an external execution agent
    #[allow(missing_docs)]
    Agent {
        #[command(subcommand)]
        action: OsAgentAction,
    },
    /// Manage an agent framework
    #[allow(missing_docs)]
    Framework {
        #[command(subcommand)]
        action: OsFrameworkAction,
    },
}

/// Actions of `susi os manage service`.
#[derive(Debug, Subcommand)]
pub enum OsServiceAction {
    #[allow(missing_docs)]
    Status,
    #[allow(missing_docs)]
    Start { name: String },
    #[allow(missing_docs)]
    Stop { name: String },
    #[allow(missing_docs)]
    Restart { name: String },
    #[allow(missing_docs)]
    Logs {
        name: String,
        #[arg(short = 'n', long, default_value_t = 50)]
        lines: usize,
    },
}

/// Actions of `susi os manage model`.
#[derive(Debug, Subcommand)]
pub enum OsModelAction {
    #[allow(missing_docs)]
    List,
    #[allow(missing_docs)]
    Doctor { name: Option<String> },
    /// Download a GGUF model from an HTTPS URL into SUSI-managed storage,
    /// `starter`: the audited local starter model and its tokenizer, or
    /// `embeddings`: the semantic-search embedding model
    #[allow(missing_docs)]
    Install { source: String },
    /// Remove a GGUF model and paired tokenizer from SUSI-managed storage
    #[allow(missing_docs)]
    Uninstall { name: String },
    /// Load a model into the daemon's inference cache (warm start)
    #[allow(missing_docs)]
    Load { name: String },
    /// Evict a model from the daemon's inference cache, freeing its memory
    #[allow(missing_docs)]
    Unload { name: String },
    #[allow(missing_docs)]
    Prefer { name: String },
    #[allow(missing_docs)]
    Configure {
        name: String,
        file: std::path::PathBuf,
    },
    #[allow(missing_docs)]
    Reset { name: String },
}

/// Actions of `susi os manage mcp`.
#[derive(Debug, Subcommand)]
pub enum OsMcpAction {
    #[allow(missing_docs)]
    List,
    #[allow(missing_docs)]
    Doctor { name: Option<String> },
    /// Install/admit a managed MCP server into the active configuration
    #[allow(missing_docs)]
    Install { name: String },
    /// Uninstall/remove a managed MCP server from the active configuration
    #[allow(missing_docs)]
    Uninstall { name: String },
    #[allow(missing_docs)]
    Configure {
        name: String,
        file: std::path::PathBuf,
    },
    #[allow(missing_docs)]
    Reset { name: String },
    #[allow(missing_docs)]
    Status,
}

/// Actions of `susi os manage agent`.
#[derive(Debug, Subcommand)]
pub enum OsAgentAction {
    #[allow(missing_docs)]
    List,
    #[allow(missing_docs)]
    Doctor { name: Option<String> },
    #[allow(missing_docs)]
    Configure {
        name: String,
        file: std::path::PathBuf,
    },
    #[allow(missing_docs)]
    Reset { name: String },
    #[allow(missing_docs)]
    Run {
        name: String,
        #[arg(long)]
        wait: bool,
        prompt: String,
    },
    #[allow(missing_docs)]
    Tasks,
    #[allow(missing_docs)]
    Cancel { task_id: String },
}

/// Actions of `susi os manage framework`.
#[derive(Debug, Subcommand)]
pub enum OsFrameworkAction {
    #[allow(missing_docs)]
    List,
    #[allow(missing_docs)]
    Doctor { name: Option<String> },
    #[allow(missing_docs)]
    Configure {
        name: String,
        file: std::path::PathBuf,
    },
    #[allow(missing_docs)]
    Reset { name: String },
    #[allow(missing_docs)]
    Run {
        name: String,
        #[arg(long)]
        wait: bool,
        prompt: String,
    },
    #[allow(missing_docs)]
    Tasks,
    #[allow(missing_docs)]
    Cancel { task_id: String },
}
