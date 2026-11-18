//! Inter-app permission and message broker CLI.
use crate::anyhow::{bail, Result};
use std::path::Path;
pub use susi_cli_defs::broker::BrokerCommands;

pub fn execute(action: Option<BrokerCommands>, workspace: &Path) -> Result<()> {
    let broker = susi_core::broker::IpcBroker::global();
    match action {
        Some(BrokerCommands::Grant {
            grantor,
            grantee,
            resource,
            action,
            ttl_secs,
        }) => {
            let scope = susi_core::broker::PermissionScope::new(resource, action);
            let grant =
                broker.grant_and_record(&grantor, &grantee, scope, ttl_secs, Some(workspace));
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({"granted": grant}))?
            );
        }
        Some(BrokerCommands::Request {
            requester,
            grantor,
            resource,
            action,
            ttl_secs,
        }) => {
            let scope = susi_core::broker::PermissionScope::new(resource, action);
            let req = broker.request(&requester, &grantor, scope, ttl_secs);
            println!("{}", serde_json::to_string_pretty(&req)?);
        }
        Some(BrokerCommands::Negotiate {
            request_id,
            actor,
            approve,
            deny,
        }) => {
            if approve == deny {
                bail!("specify exactly one of --approve or --deny");
            }
            let resolved = broker
                .negotiate(&request_id, &actor, approve, Some(workspace))
                .map_err(|e| crate::anyhow::anyhow!("{e}"))?;
            println!("{}", serde_json::to_string_pretty(&resolved)?);
        }
        Some(BrokerCommands::Pending { grantor }) => {
            let pending = broker.pending_requests(grantor.as_deref());
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({"pending": pending}))?
            );
        }
        Some(BrokerCommands::Check {
            grantee,
            resource,
            action,
        }) => {
            let scope = susi_core::broker::PermissionScope::new(resource, action);
            let permitted = broker.is_permitted(&grantee, &scope);
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "grantee": grantee,
                    "resource": scope.resource,
                    "action": scope.action,
                    "permitted": permitted,
                }))?
            );
        }
        Some(BrokerCommands::Revoke {
            grantee,
            resource,
            action,
        }) => {
            let scope = susi_core::broker::PermissionScope::new(resource, action);
            let removed = broker.revoke(&grantee, &scope);
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({"revoked": removed.is_some()}))?
            );
        }
        Some(BrokerCommands::List { grantee }) => {
            let grants = broker.grants_for(&grantee);
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({"grantee": grantee, "grants": grants})
                )?
            );
        }
        Some(BrokerCommands::Send {
            from,
            to,
            topic,
            payload,
        }) => {
            let payload: serde_json::Value = serde_json::from_str(&payload)
                .map_err(|e| crate::anyhow::anyhow!("invalid payload JSON: {e}"))?;
            let msg = broker
                .send(&from, &to, &topic, payload)
                .map_err(|e| crate::anyhow::anyhow!("{e}"))?;
            println!("{}", serde_json::to_string_pretty(&msg)?);
        }
        Some(BrokerCommands::Receive { recipient, limit }) => {
            let msgs = broker.receive(&recipient, limit);
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({"recipient": recipient, "messages": msgs})
                )?
            );
        }
        None => bail!("broker subcommand required"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::cli::defs::Cli;
    use clap::{CommandFactory, Parser};

    /// Builds the whole `susi` command tree with clap's debug assertions so
    /// any duplicate short/long flag in any subcommand fails here instead of
    /// panicking at runtime in debug builds.
    #[test]
    fn full_cli_tree_passes_clap_debug_asserts() {
        Cli::command().debug_assert();
    }

    #[test]
    fn broker_grant_and_send_keep_long_and_remaining_short_flags() {
        for args in [
            &[
                "susi",
                "broker",
                "grant",
                "--grantor",
                "susi",
                "--grantee",
                "app",
                "--resource",
                "r",
                "--action",
                "a",
            ][..],
            &[
                "susi",
                "broker",
                "grant",
                "--grantor",
                "susi",
                "-g",
                "app",
                "-r",
                "r",
                "-a",
                "a",
                "-t",
                "60",
            ],
            &[
                "susi", "broker", "send", "-f", "a", "-t", "b", "--topic", "x", "-p", "{}",
            ],
        ] {
            Cli::try_parse_from(args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
        }
    }

    #[test]
    fn broker_execute_rejects_none_and_conflicting_negotiate_flags() {
        let ws = std::path::Path::new(".");
        assert!(super::execute(None, ws)
            .unwrap_err()
            .to_string()
            .contains("broker subcommand required"));
        let err = super::execute(
            Some(super::BrokerCommands::Negotiate {
                request_id: "req-1".into(),
                actor: "susi".into(),
                approve: true,
                deny: true,
            }),
            ws,
        )
        .unwrap_err();
        assert!(err
            .to_string()
            .contains("specify exactly one of --approve or --deny"));
    }

    #[test]
    fn broker_execute_rejects_invalid_json_payload_on_send() {
        let ws = std::path::Path::new(".");
        let err = super::execute(
            Some(super::BrokerCommands::Send {
                from: "alice".into(),
                to: "bob".into(),
                topic: "ping".into(),
                payload: "{invalid-json".into(),
            }),
            ws,
        )
        .unwrap_err();
        assert!(err.to_string().contains("invalid payload JSON"));
    }
}
