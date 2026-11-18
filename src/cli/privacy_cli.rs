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
            // Mode changes are served by `susi-plug-host` on
            // `daemon.privacy.mode` (sticky file + routing preference).
            let report = susi_daemon::plug_forward::call(
                susi_core::plane_bus::daemon::privacy_mode(&mode),
                susi_core::plane_bus::topics::DAEMON_PRIVACY_MODE,
            )
            .map_err(crate::anyhow::Error::msg)?;
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
