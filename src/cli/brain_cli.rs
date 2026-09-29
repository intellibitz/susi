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
    /// Run the fixed prompt set against a fake or configured engine; record evidence
    Bench {
        /// Write JSONL evidence under this path (default: ~/.susi/brain_bench.jsonl)
        #[arg(long)]
        out: Option<std::path::PathBuf>,
    },
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
                "budget": susi_gemi::engines::cost::Budget::from_env().label(),
                "failure_streaks": store
                    .failure_streaks()
                    .into_iter()
                    .map(|(key, h)| serde_json::json!({
                        "key": key,
                        "kind": h.kind,
                        "consecutive": h.consecutive,
                        "last_unix": h.last_unix,
                    }))
                    .collect::<Vec<_>>(),
                "providers_with_evidence": providers,
                "ranking_by_task_class": classes,
                "preferred_cloud": pref.preferred_cloud,
                "policy_override": pref.policy_override,
                "auto_switched_from": pref.auto_switched_from,
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
        BrainCommands::Bench { out } => {
            use std::time::Duration;
            use susi_gemi::benchmark::BENCHMARK_PROMPTS;
            use susi_gemi::engine_benchmark::{
                run_engine_benchmark, write_bench_evidence, FakeProvider,
            };
            let fake = FakeProvider {
                engine: "fake-engine".into(),
                model: "fake-model".into(),
                reply: "benchmark reply tokens one two three".into(),
                latency: Duration::from_millis(5),
                fail: false,
            };
            let samples = run_engine_benchmark(&fake, BENCHMARK_PROMPTS);
            let path =
                out.unwrap_or_else(|| susi_paths::SusiDirs::config_dir().join("brain_bench.jsonl"));
            write_bench_evidence(&path, &samples).map_err(anyhow::Error::msg)?;
            print_json(&serde_json::json!({
                "evidence_path": path,
                "samples": samples,
            }))?;
        }
    }
    Ok(())
}
