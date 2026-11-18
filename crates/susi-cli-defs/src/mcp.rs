//! `clap` definitions for the `mcp` command family, moved verbatim from
//! `src/cli/mcp_cli.rs` (D-SP-4 Stage 4c). The handlers stay in the root
//! crate, which re-exports these types from `src/cli/mcp_cli.rs`.
//!
//! A variant marked `#[allow(missing_docs)]` keeps undocumented fields (or is
//! hidden) on purpose: clap turns every `///` comment into `--help` text, and
//! this move must leave the visible help byte-identical.
use clap::Subcommand;

/// Subcommands of `susi mcp`: leading MCP tool servers and the native MCP stdio endpoint.
#[derive(Debug, Subcommand)]
pub enum McpCommands {
    /// Run SUSI's native MCP stdio server (default when bare `susi mcp`)
    Serve,
    /// Show the top MCP tool servers and enablement/readiness
    List,
    /// Check launchers and required env vars (does not start servers)
    #[allow(missing_docs)]
    Doctor { server: Option<String> },
    /// Show setup instructions and the effective adapter
    #[allow(missing_docs)]
    Setup { server: String },
    /// Write the leading MCP into ~/.susi/mcp_config.json
    #[allow(missing_docs)]
    Enable { server: String },
    /// Remove a leading MCP from ~/.susi/mcp_config.json
    #[allow(missing_docs)]
    Disable { server: String },
    /// Override runner/args/env_keys/package via JSON file
    #[allow(missing_docs)]
    Configure {
        server: String,
        file: std::path::PathBuf,
    },
    /// Remove a host override and restore the bundled leading MCP entry
    #[allow(missing_docs)]
    Reset { server: String },
    /// Show enablement status for the top MCP servers
    Status,
    /// Verify the pinned GMCP normative requirements ledger
    /// (.agents/gmcp): vendored corpus hashes, verbatim clauses, and
    /// complete row classification — the shipped production path for
    /// VC-203-001. `--drift` instead refetches every pinned upstream and
    /// fails closed on a changed or unreachable source.
    Ledger {
        /// Check upstream standards drift instead of the local corpus
        #[arg(long)]
        drift: bool,
        /// With `--drift`: compare this pre-fetched corpus directory
        /// (keyed by manifest document name) instead of the network —
        /// the deterministic path tests and air-gapped checks use.
        #[arg(long, requires = "drift")]
        refetched: Option<std::path::PathBuf>,
    },
    /// Invoke a tool on a peer MCP endpoint (`host:port`) over the
    /// session-aware channel — the same path cluster replication uses.
    Call {
        /// Peer address, e.g. 127.0.0.1:9093 (canonical GMCP-HTTP + port_offset)
        addr: String,
        /// Tool name as exposed by the peer's tools/list
        tool: String,
        /// JSON arguments object, e.g. '{"coordinator":"node-a"}' (default {})
        #[arg(default_value = "{}")]
        args: String,
        /// Bearer token (default: cluster peer bearer for remote
        /// members, ~/.susi/api_token for loopback)
        #[arg(long)]
        token: Option<String>,
    },
    /// List the tools a peer MCP endpoint exposes (`tools/list` over the
    /// session-aware channel) — discover the surface before `mcp call`.
    Tools {
        /// Peer address, e.g. 127.0.0.1:9093 (canonical GMCP-HTTP + port_offset)
        addr: String,
        /// Bearer token (default: cluster peer bearer for remote
        /// members, ~/.susi/api_token for loopback)
        #[arg(long)]
        token: Option<String>,
    },
    /// Discover a peer MCP server's protocol revision, serverInfo, and capabilities (`VC-203-017`).
    Discover {
        /// Peer address, e.g. 127.0.0.1:9093
        addr: String,
        /// Negotiate stateful 2025-11-25 initialize instead of stateless 2026-07-28 server/discover
        #[arg(long)]
        legacy: bool,
        /// Bearer token
        #[arg(long)]
        token: Option<String>,
    },
    /// List static resources (and optional URI templates) exposed by a peer MCP endpoint (`VC-203-017`).
    Resources {
        /// Peer address, e.g. 127.0.0.1:9093
        addr: String,
        /// Include RFC 6570 resourceTemplates in output
        #[arg(long)]
        templates: bool,
        /// Bearer token
        #[arg(long)]
        token: Option<String>,
    },
    /// Read a resource URI (or headlessly inspect a `ui://` MCP App resource) on a peer MCP endpoint (`VC-203-017`).
    Read {
        /// Peer address, e.g. 127.0.0.1:9093
        addr: String,
        /// Resource URI (e.g. memo://welcome or ui://app/card)
        uri: String,
        /// Inspect `ui://` MCP Apps CSP, permissions, and domain metadata headlessly
        #[arg(long)]
        app: bool,
        /// Bearer token
        #[arg(long)]
        token: Option<String>,
    },
    /// List prompts exposed by a peer MCP endpoint (`VC-203-017`).
    Prompts {
        /// Peer address, e.g. 127.0.0.1:9093
        addr: String,
        /// Bearer token
        #[arg(long)]
        token: Option<String>,
    },
    /// Materialize a prompt on a peer MCP endpoint (`VC-203-017`).
    Prompt {
        /// Peer address, e.g. 127.0.0.1:9093
        addr: String,
        /// Prompt name
        name: String,
        /// Optional JSON arguments object (default {})
        #[arg(default_value = "{}")]
        args: String,
        /// Bearer token
        #[arg(long)]
        token: Option<String>,
    },
    /// Request argument completions (`ref/prompt` or `ref/resource`) on a peer MCP endpoint (`VC-203-017`).
    Complete {
        /// Peer address, e.g. 127.0.0.1:9093
        addr: String,
        /// Reference kind: `prompt` or `resource`
        kind: String,
        /// Prompt name or resource template URI
        target: String,
        /// Argument name
        arg: String,
        /// Current argument prefix value
        value: String,
        /// Bearer token
        #[arg(long)]
        token: Option<String>,
    },
    /// Inspect, update, or cancel an Official MCP Task on a peer MCP endpoint (`VC-203-017`).
    Task {
        /// Peer address, e.g. 127.0.0.1:9093
        addr: String,
        /// Task operation: `get`, `update`, or `cancel`
        op: String,
        /// Durable task identifier
        task_id: String,
        /// JSON inputResponses object when `op` is `update` (default {})
        #[arg(default_value = "{}")]
        responses: String,
        /// Bearer token
        #[arg(long)]
        token: Option<String>,
    },
}
