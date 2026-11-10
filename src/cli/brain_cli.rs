//! `susi brain`: which model susi will trust for which kind of work, and why.
//! Rankings come from recorded outcomes (`brain_evidence.json`), not a fixed
//! vendor order.
use crate::anyhow::{Error, Result};
use crate::cli_json::print_json;
pub use susi_cli_defs::brain::BrainCommands;
use susi_daemon::provider_ops as ops;
use susi_gemi::engines::brain::{self, TaskClass};
use susi_gemi::engines::cost::Budget;

pub fn execute(action: Option<BrainCommands>) -> Result<()> {
    match action.unwrap_or_default() {
        BrainCommands::Status { top, class, json } => {
            if json {
                // `susi_daemon::provider_ops` builds every machine-readable
                // brain document; the MCP `brain_*` tools return the same.
                print_json(&ops::brain_status(top, class.as_deref()))?;
            } else {
                let store = brain::load();
                let providers = store.providers();
                let pref = susi_gemi::engines::routing::InferenceRouter::load_preference();
                let cooled_providers =
                    susi_gemi::engines::routing::InferenceRouter::cooled_providers();
                let budget = Budget::from_env();
                println!("=== SUSI BRAIN STATUS ===");
                println!(
                    "Principle: local is the floor, cloud is the ceiling; evidence beats priors"
                );
                println!(
                    "Budget: {} | Preferred Cloud: {}",
                    budget.label(),
                    pref.preferred_cloud.as_deref().unwrap_or("none")
                );
                if !cooled_providers.is_empty() {
                    let cooled_list: Vec<&str> = cooled_providers
                        .iter()
                        .map(|c| c.provider.as_str())
                        .collect();
                    println!("Cooled Providers: {}", cooled_list.join(", "));
                }
                println!();

                for c in TaskClass::ALL {
                    if let Some(target) = &class {
                        if !c.label().eq_ignore_ascii_case(target) {
                            continue;
                        }
                    }
                    println!("[Task Class: {}]", c.label().to_uppercase());
                    let mut ranked = store.rank(&providers, c);
                    ranked.truncate(top);
                    if ranked.is_empty() {
                        println!("  (no evidence recorded yet)");
                    } else {
                        for (i, r) in ranked.iter().enumerate() {
                            let success_str = r
                                .success_rate
                                .map(|sr| format!("{:.0}%", sr * 100.0))
                                .unwrap_or_else(|| "n/a".into());
                            let latency_str = r
                                .avg_latency_ms
                                .map(|ms| format!("{}ms", ms))
                                .unwrap_or_else(|| "n/a".into());
                            let floor_str = if r.meets_floor {
                                "floor:ok"
                            } else {
                                "below_floor"
                            };
                            let unfit_str = if r.unfit { " [UNFIT]" } else { "" };
                            println!(
                                "  {:2}. {:<20} score: {:<5.3}  tier: {:<4}  success: {:<4}  avg: {:<6}  {}{}",
                                i + 1,
                                r.provider,
                                r.score,
                                r.cost_tier,
                                success_str,
                                latency_str,
                                floor_str,
                                unfit_str
                            );
                        }
                    }
                    println!();
                }
            }
        }
        BrainCommands::Matrix => print_json(&ops::brain_matrix().map_err(Error::msg)?)?,
        BrainCommands::Pareto { class } => print_json(&ops::brain_pareto(class.as_deref()))?,
        BrainCommands::Cache => print_json(&ops::brain_cache())?,
        BrainCommands::Budget {
            free_only,
            allow_paid,
            level,
            max_spend_microusd,
            max_tokens,
            max_requests,
            max_parallel,
            new_window,
        } => {
            let doc = ops::brain_budget(&ops::BudgetUpdate {
                level,
                free_only,
                allow_paid,
                max_spend_microusd,
                max_tokens,
                max_requests,
                max_parallel,
                new_window,
            })
            .map_err(Error::msg)?;
            for line in doc["lines"].as_array().into_iter().flatten() {
                println!("{}", line.as_str().unwrap_or_default());
            }
        }
        BrainCommands::Classify { prompt } => print_json(&ops::brain_classify(&prompt.join(" ")))?,
        BrainCommands::Explain { prompt } => {
            print_json(&ops::brain_explain(&prompt.join(" ")).map_err(Error::msg)?)?
        }
        BrainCommands::Export { out } => {
            let doc = ops::brain_export(out.as_deref()).map_err(Error::msg)?;
            match doc
                .get("message")
                .and_then(|m| m.as_str())
                .filter(|_| out.is_some())
            {
                Some(message) => println!("{message}"),
                None => print_json(&doc)?,
            }
        }
        BrainCommands::Import { file } => {
            let doc = ops::brain_import(&file).map_err(Error::msg)?;
            println!("{}", doc["message"].as_str().unwrap_or_default());
        }
        BrainCommands::Reset => {
            let doc = ops::brain_reset().map_err(Error::msg)?;
            println!("{}", doc["message"].as_str().unwrap_or_default());
        }
        BrainCommands::Bench { out } => print_json(&ops::brain_bench(out).map_err(Error::msg)?)?,
        BrainCommands::Probe {
            provider,
            no_record,
        } => print_json(&ops::brain_probe(&provider, no_record).map_err(Error::msg)?)?,
    }
    Ok(())
}
