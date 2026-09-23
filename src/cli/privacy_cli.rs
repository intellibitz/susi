//! Privacy / MAC capability CLI.
use anyhow::{bail, Result};
use clap::Subcommand;
use susi_core::mac_policy::{actions, MacPolicy, PrivacyMode};

#[derive(Debug, Subcommand)]
pub enum PrivacyCommands {
    /// Show privacy mode and active capability grants.
    Status,
    /// Set privacy mode: balanced | local_only | open
    Mode { mode: String },
    /// Grant a cryptographic capability token.
    Grant {
        #[arg(short, long, default_value = "susi")]
        subject: String,
        #[arg(short, long)]
        action: String,
        #[arg(short, long, default_value = "*")]
        resource: String,
        #[arg(short, long)]
        ttl_secs: Option<u64>,
    },
    /// Revoke a capability grant.
    Revoke {
        #[arg(short, long, default_value = "susi")]
        subject: String,
        #[arg(short, long)]
        action: String,
        #[arg(short, long, default_value = "*")]
        resource: String,
    },
    /// Explicit consent for network egress (and cloud inference under local_only).
    Consent {
        #[arg(long)]
        egress: bool,
        #[arg(short, long, default_value = "susi")]
        subject: String,
        #[arg(short, long, default_value_t = 3600)]
        ttl_secs: u64,
    },
}

pub fn execute(action: Option<PrivacyCommands>) -> Result<()> {
    // Ensure policy is wired from host config/key.
    let substrate = crate::susi_paths::SusiDirs::substrate_home();
    susi_daemon::privacy::wire_mac_policy(&substrate);

    match action.unwrap_or(PrivacyCommands::Status) {
        PrivacyCommands::Status => {
            println!(
                "{}",
                serde_json::to_string_pretty(&MacPolicy::global().status_json())?
            );
        }
        PrivacyCommands::Mode { mode } => {
            let m = PrivacyMode::parse(&mode);
            susi_daemon::privacy::persist_privacy_mode(m)?;
            // Also align inference routing sticky preference for local_only.
            if matches!(m, PrivacyMode::LocalOnly) {
                let pref_path = substrate.join("routing_preference.json");
                let body = serde_json::json!({
                    "policy_override": "local_only",
                    "preferred_cloud": null
                });
                let _ = std::fs::write(pref_path, serde_json::to_string_pretty(&body)?);
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "mode": m.as_str(),
                    "mandatory_sandbox": MacPolicy::global().mandatory_sandbox(),
                    "blocks_cloud_inference": MacPolicy::global().blocks_cloud_inference(),
                }))?
            );
        }
        PrivacyCommands::Grant {
            subject,
            action,
            resource,
            ttl_secs,
        } => {
            let token = MacPolicy::global().grant(&subject, &action, &resource, ttl_secs);
            println!("{}", serde_json::to_string_pretty(&token)?);
        }
        PrivacyCommands::Revoke {
            subject,
            action,
            resource,
        } => {
            let ok = MacPolicy::global().revoke(&subject, &action, &resource);
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({"revoked": ok}))?
            );
        }
        PrivacyCommands::Consent {
            egress,
            subject,
            ttl_secs,
        } => {
            if !egress {
                bail!("specify --egress to authorize network egress / cloud inference consent");
            }
            let tokens = MacPolicy::global().consent_egress(&subject, ttl_secs);
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "consented": true,
                    "actions": [actions::NETWORK_EGRESS, actions::CLOUD_INFERENCE],
                    "tokens": tokens,
                }))?
            );
        }
    }
    Ok(())
}
