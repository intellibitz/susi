//! `susi brain`: which model susi will trust for which kind of work, and why.
//! Rankings come from recorded outcomes (`brain_evidence.json`), not a fixed
//! vendor order.
use crate::cli_json::print_json;
use anyhow::Result;
use clap::Subcommand;
use susi_gemi::engines::brain::{self, TaskClass};

#[derive(Debug, Subcommand)]
pub enum BrainCommands {
    /// Show the evidence-ranked providers for every task class (default)
    Status,
    /// Show which task class a prompt is routed as
    Classify {
        #[arg(trailing_var_arg = true)]
        prompt: Vec<String>,
    },
    /// Forget all recorded outcomes (new account, changed model lineup)
    Reset,
}

pub fn execute(action: Option<BrainCommands>) -> Result<()> {
    match action.unwrap_or(BrainCommands::Status) {
        BrainCommands::Status => {
            let store = brain::load();
            let providers = store.providers();
            let classes: serde_json::Map<String, serde_json::Value> = TaskClass::ALL
                .iter()
                .map(|c| {
                    (
                        c.label().to_string(),
                        serde_json::to_value(store.rank(&providers, *c)).unwrap_or_default(),
                    )
                })
                .collect();
            let pref = susi_gemi::engines::routing::InferenceRouter::load_preference();
            let cooled_providers = susi_gemi::engines::routing::InferenceRouter::cooled_providers();
            print_json(&serde_json::json!({
                "principle": "local is the floor, cloud is the ceiling; evidence beats priors",
                "providers_with_evidence": providers,
                "ranking_by_task_class": classes,
                "preferred_cloud": pref.preferred_cloud,
                "policy_override": pref.policy_override,
                "cooled_providers": cooled_providers,
            }))?;
        }
        BrainCommands::Classify { prompt } => {
            let text = prompt.join(" ");
            print_json(&serde_json::json!({ "task_class": TaskClass::classify(&text).label() }))?;
        }
        BrainCommands::Reset => {
            brain::reset()?;
            println!("brain evidence cleared");
        }
    }
    Ok(())
}
