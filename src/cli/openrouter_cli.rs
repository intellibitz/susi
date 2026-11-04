//! Deterministic OpenRouter control plane (no daemon required).
use crate::cli_json::print_json;
use anyhow::{Error, Result};
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum OpenRouterCommands {
    /// Show OpenRouter status and curated routes
    List,
    /// Check key + endpoint readiness (does not call a paid model unless --live)
    Doctor {
        /// Also hit GET /models to verify the key works
        #[arg(long)]
        live: bool,
    },
    /// Show setup instructions and effective wiring
    Setup,
    /// Prefer OpenRouter for cloud routing; optionally pin a model/route id
    Prefer {
        /// Catalog id (e.g. claude-sonnet) or raw OpenRouter model slug
        model: Option<String>,
        /// Clear the pinned model (keep cloud preference)
        #[arg(long)]
        clear_model: bool,
    },
    /// Paid probe: short completion via the effective OpenRouter model
    Probe {
        #[arg(default_value = "Reply with exactly: ok")]
        prompt: String,
    },
    /// List curated routes, or live provider model ids with --live
    Models {
        #[arg(long)]
        live: bool,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
}

pub fn execute(action: Option<OpenRouterCommands>) -> Result<()> {
    // `susi_daemon::provider_ops` builds every document; the MCP
    // `provider_*` tools return the same ones.
    use susi_daemon::provider_ops::{self as ops, Vendor};
    let action = action.unwrap_or(OpenRouterCommands::List);
    match action {
        OpenRouterCommands::List => {
            print_json(&ops::list(Vendor::OpenRouter).map_err(Error::msg)?)?
        }
        OpenRouterCommands::Doctor { live } => {
            for doc in ops::doctor(Vendor::OpenRouter, None, live).map_err(Error::msg)? {
                print_json(&doc)?;
            }
        }
        OpenRouterCommands::Setup => {
            print_json(&ops::setup(Vendor::OpenRouter, None).map_err(Error::msg)?)?
        }
        OpenRouterCommands::Prefer { model, clear_model } => println!(
            "{}",
            ops::prefer(Vendor::OpenRouter, model.as_deref(), clear_model).map_err(Error::msg)?
        ),
        OpenRouterCommands::Probe { prompt } => {
            print_json(&ops::probe("openrouter", None, &prompt).map_err(Error::msg)?)?
        }
        OpenRouterCommands::Models { live, limit } => {
            print_json(&ops::openrouter_models(live, limit).map_err(Error::msg)?)?
        }
    }
    Ok(())
}
