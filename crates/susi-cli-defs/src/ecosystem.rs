//! `clap` definitions for the `ecosystem` command family, moved verbatim from
//! `src/cli/ecosystem_cli.rs` (D-SP-4 Stage 4c). The handlers stay in the root
//! crate, which re-exports these types from `src/cli/ecosystem_cli.rs`.
//!
//! A variant marked `#[allow(missing_docs)]` keeps undocumented fields (or is
//! hidden) on purpose: clap turns every `///` comment into `--help` text, and
//! this move must leave the visible help byte-identical.
use clap::Subcommand;
use std::path::PathBuf;

/// Subcommands of `susi ecosystem`: local AI engines, runtimes and hardware backends.
#[derive(Debug, Subcommand)]
pub enum EcosystemCommands {
    /// Scan hardware and installed local AI ecosystems now (default)
    Scan,
    /// Show what the last startup scan found
    Status,
    /// Start an installed engine that needs no further input (e.g. ollama, local-ai)
    Start {
        /// Engine id from `susi ecosystem scan`
        id: String,
    },
    /// Cloud-AI ecosystem knowledge base: show, search, list, check
    #[allow(missing_docs)]
    Kb {
        // Boxed: `search` carries several strings and would grow `Commands`.
        #[command(subcommand)]
        action: Option<Box<KbCommands>>,
    },
    /// Explain a component, standard or protocol from the knowledge base:
    /// what it is, its versions, who implements it, how susi uses it
    Explain {
        /// Entity id (e.g. `openai-chat-completions`, `mcp`, `a2a`)
        id: String,
        /// Read a single store dir instead of the layered bundled+user store
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}

/// Subcommands of `susi ecosystem kb`: the cloud-AI-ecosystem knowledge base.
#[derive(Debug, Subcommand)]
pub enum KbCommands {
    /// List entity ids, kinds and names
    List {
        /// Restrict to one kind: component|vendor|standard|protocol|spec-version|capability
        #[arg(long)]
        kind: Option<String>,
        /// Read a single store dir instead of the layered bundled+user store
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Show one entity by id, with its incoming and outgoing relations
    #[allow(missing_docs)]
    Show {
        /// Entity id (e.g. `openai`, `cap-tool-calling`)
        id: String,
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Search entities by id/name substring, with filters
    #[allow(missing_docs)]
    Search {
        /// Case-insensitive substring matched against id and name
        query: Option<String>,
        /// Restrict to one kind
        #[arg(long)]
        kind: Option<String>,
        /// Keep components having this capability (any vendor spelling:
        /// "tool calling", "function calling", "tools" all match)
        #[arg(long)]
        capability: Option<String>,
        /// Keep the vendor itself and components it `provides`
        #[arg(long)]
        vendor: Option<String>,
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Run schema + consistency checks over the store (nonzero exit on issues)
    #[allow(missing_docs)]
    Check {
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}
