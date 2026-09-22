//! Deterministic OpenRouter control plane (no daemon required).
use anyhow::Result;
use clap::Subcommand;
use susi_gemi::openrouter::OpenRouterManager;

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

fn print_json(value: &impl serde::Serialize) -> Result<()> {
    println!(
        "{}",
        susi_agents::external::redact(&serde_json::to_string_pretty(value)?)
    );
    Ok(())
}

pub fn execute(action: Option<OpenRouterCommands>) -> Result<()> {
    let action = action.unwrap_or(OpenRouterCommands::List);
    let manager = OpenRouterManager::new()?;
    match action {
        OpenRouterCommands::List => {
            let routes = OpenRouterManager::catalog()?;
            print_json(&serde_json::json!({
                "status": manager.status(),
                "routes": routes,
            }))?;
        }
        OpenRouterCommands::Doctor { live } => {
            let detail = manager.doctor()?;
            print_json(&serde_json::json!({
                "vendor": "openrouter",
                "prerequisites_present": true,
                "detail": detail,
            }))?;
            if live {
                let ids = susi_gemi::openrouter_ext::list_live(5)?;
                print_json(&serde_json::json!({
                    "live_models_sample": ids,
                    "detail": "OpenRouter /models reachable"
                }))?;
            }
        }
        OpenRouterCommands::Setup => print_json(&manager.setup())?,
        OpenRouterCommands::Prefer { model, clear_model } => {
            println!(
                "{}",
                susi_gemi::openrouter_ext::prefer(model.as_deref(), clear_model)?
            );
        }
        OpenRouterCommands::Probe { prompt } => {
            let output = susi_gemi::openrouter_ext::probe(&prompt)?;
            print_json(&serde_json::json!({
                "vendor": "openrouter",
                "model": manager.effective_model(),
                "output": output,
            }))?;
        }
        OpenRouterCommands::Models { live, limit } => {
            if live {
                let ids = susi_gemi::openrouter_ext::list_live(limit)?;
                print_json(&serde_json::json!({
                    "source": "live",
                    "count": ids.len(),
                    "models": ids,
                }))?;
            } else {
                print_json(&OpenRouterManager::catalog()?)?;
            }
        }
    }
    Ok(())
}
