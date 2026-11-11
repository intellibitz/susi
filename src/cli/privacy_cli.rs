//! Privacy / MAC capability CLI.
use crate::anyhow::{bail, Result};
use susi_core::mac_policy::{actions, MacPolicy};

pub use susi_cli_defs::privacy::*;

pub fn execute(action: Option<PrivacyCommands>) -> Result<()> {
    // Ensure policy is wired from host config/key.
    let substrate = susi_paths::SusiDirs::substrate_home();
    susi_daemon::privacy::wire_mac_policy(&substrate);

    match action.unwrap_or(PrivacyCommands::Status) {
        PrivacyCommands::Status => {
            println!(
                "{}",
                serde_json::to_string_pretty(&MacPolicy::global().status_json())?
            );
        }
        PrivacyCommands::Mode { mode } => {
            // The one mode-change path the socket's `privacy_mode` also takes:
            // refuses an unknown mode, persists the sticky file a running
            // daemon reads per operation, and pins inference routing to local
            // under local_only and host_only.
            let report = susi_daemon::privacy::set_privacy_mode(&mode)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        PrivacyCommands::Grant {
            subject,
            action,
            resource,
            ttl_secs,
        } => {
            let token = MacPolicy::global().grant(&subject, &action, &resource, ttl_secs)?;
            println!("{}", serde_json::to_string_pretty(&token)?);
        }
        PrivacyCommands::Revoke {
            subject,
            action,
            resource,
        } => {
            let ok = MacPolicy::global().revoke(&subject, &action, &resource)?;
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
            let tokens = MacPolicy::global().consent_egress(&subject, ttl_secs)?;
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
