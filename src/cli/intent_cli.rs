//! Semantic intent bus CLI.
use anyhow::{bail, Result};
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum IntentCommands {
    /// Advertise a provider capability (NL intent).
    Advertise {
        #[arg(short, long)]
        from: String,
        #[arg(short, long)]
        intent: String,
    },
    /// Publish a need and print matched providers.
    Need {
        #[arg(short, long)]
        from: String,
        #[arg(short, long)]
        intent: String,
        #[arg(short, long, default_value_t = 0.2)]
        min_score: f32,
        #[arg(short, long, default_value_t = 5)]
        limit: usize,
    },
    /// List active providers and needs.
    List,
}

pub fn execute(action: Option<IntentCommands>) -> Result<()> {
    let bus = susi_core::intent_bus::IntentBus::global();
    match action {
        Some(IntentCommands::Advertise { from, intent }) => {
            let msg = bus.advertise(&from, &intent, serde_json::json!({}), None);
            println!("{}", serde_json::to_string_pretty(&msg)?);
        }
        Some(IntentCommands::Need {
            from,
            intent,
            min_score,
            limit,
        }) => {
            let (need, matches) = bus.need(
                &from,
                &intent,
                serde_json::json!({}),
                None,
                min_score,
                limit,
            );
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "need": need,
                    "matches": matches,
                }))?
            );
        }
        Some(IntentCommands::List) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "providers": bus.providers(),
                    "needs": bus.needs(),
                }))?
            );
        }
        None => bail!("intent subcommand required (advertise|need|list)"),
    }
    Ok(())
}
