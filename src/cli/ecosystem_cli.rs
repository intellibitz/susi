//! `susi ecosystem`: what local AI engines, runtimes and hardware backends
//! this host has, and explicit start of the ones susi can launch unattended.
//! `susi ecosystem kb` reads the cloud-AI-ecosystem knowledge base
//! (`susi_gemi_models::eco_*`): show, search, list, check.
use crate::cli_json::print_json;
use anyhow::{bail, Result};
use clap::Subcommand;
use std::path::PathBuf;
use susi_gemi::models::eco_query;

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
    Show {
        /// Entity id (e.g. `openai`, `cap-tool-calling`)
        id: String,
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Search entities by id/name substring, with filters
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
    Check {
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}

/// The knowledge-base queries are `susi_gemi::models::eco_query`'s — the
/// documents the MCP `ecosystem_kb` / `ecosystem_explain` tools return.
fn kb_execute(action: KbCommands) -> Result<()> {
    match action {
        KbCommands::List { kind, dir } => {
            print_json(
                &eco_query::list(kind.as_deref(), dir.as_deref()).map_err(anyhow::Error::msg)?,
            )?;
        }
        KbCommands::Show { id, dir } => {
            print_json(&eco_query::show(&id, dir.as_deref()).map_err(anyhow::Error::msg)?)?;
        }
        KbCommands::Search {
            query,
            kind,
            capability,
            vendor,
            dir,
        } => {
            let filters = eco_query::Filters {
                query: query.as_deref(),
                kind: kind.as_deref(),
                capability: capability.as_deref(),
                vendor: vendor.as_deref(),
            };
            print_json(&eco_query::search(&filters, dir.as_deref()).map_err(anyhow::Error::msg)?)?;
        }
        KbCommands::Check { dir } => {
            let report = eco_query::check(dir.as_deref()).map_err(anyhow::Error::msg)?;
            print_json(&report)?;
            let issues = report["issues"].as_array().map_or(0, Vec::len);
            if issues > 0 {
                bail!("ecosystem knowledge base has {issues} issue(s)");
            }
        }
    }
    Ok(())
}

/// `susi ecosystem explain <id>`: one entity explained end to end
/// (`eco_query::explain`).
fn explain(id: &str, dir: Option<PathBuf>) -> Result<()> {
    print_json(&eco_query::explain(id, dir.as_deref()).map_err(anyhow::Error::msg)?)?;
    Ok(())
}

pub fn execute(action: Option<EcosystemCommands>) -> Result<()> {
    match action.unwrap_or(EcosystemCommands::Scan) {
        EcosystemCommands::Scan => {
            print_json(&susi_daemon::discovery_pipeline::local_ecosystem_scan_report())?
        }
        EcosystemCommands::Status => {
            let text = susi_daemon::discovery_pipeline::last_ecosystem_scan()
                .map_err(anyhow::Error::msg)?;
            println!("{text}");
        }
        EcosystemCommands::Start { id } => {
            match susi_gemi::models::local_ecosystem::start(
                &id,
                &susi_gemi::models::local_ecosystem::HostProbe,
            ) {
                Ok(msg) => println!("{msg}"),
                Err(e) => bail!("{e}"),
            }
        }
        EcosystemCommands::Kb { action } => {
            kb_execute(action.map(|b| *b).unwrap_or(KbCommands::List {
                kind: None,
                dir: None,
            }))?
        }
        EcosystemCommands::Explain { id, dir } => explain(&id, dir)?,
    }
    Ok(())
}
