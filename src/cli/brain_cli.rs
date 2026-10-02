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
    /// Set shared cloud spending and request limits (USD expressed in microdollars)
    Budget {
        #[arg(long, conflicts_with = "allow_paid")]
        free_only: bool,
        /// Authorize paid inference within the configured limits
        #[arg(long)]
        allow_paid: bool,
        #[arg(long)]
        max_spend_microusd: Option<u64>,
        #[arg(long)]
        max_tokens: Option<u64>,
        #[arg(long)]
        max_requests: Option<u64>,
        #[arg(long)]
        max_parallel: Option<u64>,
        /// Explicitly start a new accounting window; unresolved calls forbid reset
        #[arg(long)]
        new_window: bool,
    },
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
                "shared_cloud_budget": susi_gemi_models::cloud_budget::status()?,
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
        BrainCommands::Budget {
            free_only,
            allow_paid,
            max_spend_microusd,
            max_tokens,
            max_requests,
            max_parallel,
            new_window,
        } => {
            use susi_gemi_models::cloud_budget;
            let mut policy = cloud_budget::status()?.policy;
            if free_only { policy.allow_paid = false; }
            if allow_paid { policy.allow_paid = true; }
            if let Some(limit) = max_spend_microusd { policy.max_spend_microusd = Some(limit); }
            if let Some(limit) = max_tokens { policy.max_tokens = Some(limit); }
            if let Some(limit) = max_requests { policy.max_requests = Some(limit); }
            if let Some(limit) = max_parallel { policy.max_parallel = limit; }
            cloud_budget::configure(policy, new_window)?;
            print_json(&cloud_budget::status()?)?;
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
