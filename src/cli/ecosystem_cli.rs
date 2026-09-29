//! `susi ecosystem`: what local AI engines, runtimes and hardware backends
//! this host has, and explicit start of the ones susi can launch unattended.
use crate::cli_json::print_json;
use anyhow::{bail, Result};
use clap::Subcommand;

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
    }
    Ok(())
}
