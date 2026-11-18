//! `susi ecosystem`: what local AI engines, runtimes and hardware backends
//! this host has, and explicit start of the ones susi can launch unattended.
//! `susi ecosystem kb` reads the cloud-AI-ecosystem knowledge base
//! (`susi_gemi_models::eco_*`): show, search, list, check.
use crate::anyhow::{bail, Result};
use crate::cli_json::print_json;
use std::path::PathBuf;
pub use susi_cli_defs::ecosystem::{EcosystemCommands, KbCommands};
use susi_gemi::models::eco_query;

/// The knowledge-base queries are `susi_gemi::models::eco_query`'s — the
/// documents the MCP `ecosystem_kb` / `ecosystem_explain` tools return.
fn kb_execute(action: KbCommands) -> Result<()> {
    match action {
        KbCommands::List { kind, dir } => {
            print_json(
                &eco_query::list(kind.as_deref(), dir.as_deref())
                    .map_err(crate::anyhow::Error::msg)?,
            )?;
        }
        KbCommands::Show { id, dir } => {
            print_json(&eco_query::show(&id, dir.as_deref()).map_err(crate::anyhow::Error::msg)?)?;
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
            print_json(
                &eco_query::search(&filters, dir.as_deref()).map_err(crate::anyhow::Error::msg)?,
            )?;
        }
        KbCommands::Check { dir } => {
            let report = eco_query::check(dir.as_deref()).map_err(crate::anyhow::Error::msg)?;
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
    print_json(&eco_query::explain(id, dir.as_deref()).map_err(crate::anyhow::Error::msg)?)?;
    Ok(())
}

pub fn execute(action: Option<EcosystemCommands>) -> Result<()> {
    match action.unwrap_or(EcosystemCommands::Scan) {
        EcosystemCommands::Scan => {
            print_json(&susi_daemon::discovery_pipeline::local_ecosystem_scan_report())?
        }
        EcosystemCommands::Status => {
            let text = susi_daemon::discovery_pipeline::last_ecosystem_scan()
                .map_err(crate::anyhow::Error::msg)?;
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
