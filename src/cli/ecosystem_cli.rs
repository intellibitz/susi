//! `susi ecosystem`: what local AI engines, runtimes and hardware backends
//! this host has, and explicit start of the ones susi can launch unattended.
//! `susi ecosystem kb` reads the cloud-AI-ecosystem knowledge base
//! (`susi_gemi_models::eco_*`): show, search, list, check.
use crate::cli_json::print_json;
use anyhow::{bail, Context as _, Result};
use clap::Subcommand;
use std::path::PathBuf;
use susi_gemi::models::eco_schema::{Entity, EntityKind, KnowledgeBase};
use susi_gemi::models::{eco_consistency, eco_relations, eco_store, eco_taxonomy};

#[derive(Debug, Subcommand)]
pub enum EcosystemCommands {
    /// Scan hardware and installed local AI ecosystems now (default)
    Scan,
    /// Show what the last startup scan found
    Status,
    /// Start an installed engine that needs no further input (e.g. ollama, lm-studio)
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

/// Bundled KB layer: where the signed catalog channel installs updates
/// (`data_dir()/ecosystem-bundled`; `SUSI_ECO_BUNDLED` overrides for tests).
fn bundled_dir() -> PathBuf {
    std::env::var_os("SUSI_ECO_BUNDLED")
        .map(PathBuf::from)
        .unwrap_or_else(|| susi_paths::SusiDirs::data_dir().join("ecosystem-bundled"))
}

fn load_kb(dir: Option<PathBuf>) -> Result<KnowledgeBase> {
    match dir {
        Some(d) => eco_store::load_dir(&d).with_context(|| format!("load {}", d.display())),
        None => eco_store::load(&bundled_dir(), &eco_store::default_store_dir())
            .context("load layered ecosystem store"),
    }
}

struct Filters<'a> {
    query: Option<&'a str>,
    kind: Option<&'a str>,
    capability: Option<&'a str>,
    vendor: Option<&'a str>,
}

fn matches(kb: &KnowledgeBase, e: &Entity, f: &Filters) -> bool {
    if let Some(k) = f.kind {
        if e.kind().label() != k {
            return false;
        }
    }
    if let Some(q) = f.query {
        let q = q.to_lowercase();
        if !(e.id().contains(q.as_str()) || e.name().to_lowercase().contains(q.as_str())) {
            return false;
        }
    }
    if let Some(v) = f.vendor {
        let g = eco_relations::Graph::new(kb);
        let ok = (e.kind() == EntityKind::Vendor && e.id() == v)
            || g.providers(e.id()).iter().any(|p| p.id() == v);
        if !ok {
            return false;
        }
    }
    if let Some(c) = f.capability {
        let g = eco_relations::Graph::new(kb);
        let canonical = eco_taxonomy::canonical(c).map(|c| c.id);
        let ok = g.capabilities_of(e.id()).iter().any(|cap| {
            Some(cap.id()) == canonical || cap.id() == c || cap.name().eq_ignore_ascii_case(c)
        });
        if !ok {
            return false;
        }
    }
    true
}

fn entity_json(e: &Entity) -> serde_json::Value {
    serde_json::json!({
        "id": e.id(),
        "kind": e.kind().label(),
        "name": e.name(),
    })
}

fn kb_execute(action: KbCommands) -> Result<()> {
    match action {
        KbCommands::List { kind, dir } => {
            let kb = load_kb(dir)?;
            let f = Filters {
                query: None,
                kind: kind.as_deref(),
                capability: None,
                vendor: None,
            };
            let rows: Vec<_> = kb
                .entities
                .iter()
                .filter(|e| matches(&kb, e, &f))
                .map(entity_json)
                .collect();
            print_json(&rows)?;
        }
        KbCommands::Show { id, dir } => {
            let kb = load_kb(dir)?;
            let Some(e) = kb.entity(&id) else {
                bail!("no ecosystem entity {id:?} — try `susi ecosystem kb search`");
            };
            let outgoing: Vec<_> = kb.relations.iter().filter(|r| r.from == id).collect();
            let incoming: Vec<_> = kb.relations.iter().filter(|r| r.to == id).collect();
            print_json(&serde_json::json!({
                "entity": e,
                "outgoing": outgoing,
                "incoming": incoming,
            }))?;
        }
        KbCommands::Search {
            query,
            kind,
            capability,
            vendor,
            dir,
        } => {
            let kb = load_kb(dir)?;
            let f = Filters {
                query: query.as_deref(),
                kind: kind.as_deref(),
                capability: capability.as_deref(),
                vendor: vendor.as_deref(),
            };
            let rows: Vec<_> = kb
                .entities
                .iter()
                .filter(|e| matches(&kb, e, &f))
                .map(entity_json)
                .collect();
            print_json(&serde_json::json!({ "count": rows.len(), "entities": rows }))?;
        }
        KbCommands::Check { dir } => {
            let kb = load_kb(dir)?;
            let issues = eco_consistency::check(&kb);
            print_json(&serde_json::json!({
                "entities": kb.entities.len(),
                "relations": kb.relations.len(),
                "issues": issues,
            }))?;
            if !issues.is_empty() {
                bail!("ecosystem knowledge base has {} issue(s)", issues.len());
            }
        }
    }
    Ok(())
}

pub fn execute(action: Option<EcosystemCommands>) -> Result<()> {
    match action.unwrap_or(EcosystemCommands::Scan) {
        EcosystemCommands::Scan => {
            print_json(&susi_daemon::discovery_pipeline::local_ecosystem_report())?
        }
        EcosystemCommands::Status => {
            let path = susi_paths::SusiDirs::config_dir().join("local_ecosystem.json");
            match std::fs::read_to_string(&path) {
                Ok(text) => println!("{text}"),
                Err(_) => bail!("no startup scan yet; run `susi ecosystem scan`"),
            }
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
    }
    Ok(())
}
