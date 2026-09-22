//! Inter-app permission and message broker CLI.
use anyhow::{bail, Result};
use clap::Subcommand;
use std::path::Path;

#[derive(Debug, Subcommand)]
pub enum BrokerCommands {
    /// Grant a permission scope to an identity.
    Grant {
        #[arg(short, long)]
        grantor: String,
        #[arg(short, long)]
        grantee: String,
        #[arg(short, long)]
        resource: String,
        #[arg(short, long)]
        action: String,
        #[arg(short, long)]
        ttl_secs: Option<u64>,
    },
    /// Request a permission (opens a negotiation).
    Request {
        #[arg(short = 'r', long)]
        requester: String,
        #[arg(short = 'o', long, default_value = "susi")]
        grantor: String,
        #[arg(short = 'R', long)]
        resource: String,
        #[arg(short, long)]
        action: String,
        #[arg(short, long)]
        ttl_secs: Option<u64>,
    },
    /// Resolve a pending permission request.
    Negotiate {
        #[arg(short, long)]
        request_id: String,
        #[arg(short, long)]
        actor: String,
        #[arg(long)]
        approve: bool,
        #[arg(long)]
        deny: bool,
    },
    /// List pending permission requests.
    Pending {
        #[arg(short, long)]
        grantor: Option<String>,
    },
    /// Check whether an identity holds a scope.
    Check {
        #[arg(short, long)]
        grantee: String,
        #[arg(short, long)]
        resource: String,
        #[arg(short, long)]
        action: String,
    },
    /// Revoke a permission scope from an identity.
    Revoke {
        #[arg(short, long)]
        grantee: String,
        #[arg(short, long)]
        resource: String,
        #[arg(short, long)]
        action: String,
    },
    /// List grants for an identity.
    List {
        #[arg(short, long)]
        grantee: String,
    },
    /// Send a message to another identity (requires dispatch grant).
    Send {
        #[arg(short, long)]
        from: String,
        #[arg(short, long)]
        to: String,
        #[arg(short, long)]
        topic: String,
        /// JSON payload string.
        #[arg(short, long)]
        payload: String,
    },
    /// Receive messages for an identity.
    Receive {
        #[arg(short, long)]
        recipient: String,
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
    },
}

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
                .map_err(|e| anyhow::anyhow!("{e}"))?;
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
                .map_err(|e| anyhow::anyhow!("invalid payload JSON: {e}"))?;
            let msg = broker
                .send(&from, &to, &topic, payload)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
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
