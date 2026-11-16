//! Socket reach for CLI host-operation commands (zero-UI plan B4 groups 4,
//! 5, 8 and 9): onboarding, assurance, context and IPC, and the OS view.
//!
//! Every handler calls the same library function its CLI command calls and
//! returns the document the command prints (its `--json` form where it has
//! one). Capabilities that live in another plane — the daemon's composition
//! root (`daemon.`), GEMI's ecosystem knowledge base (`gemi.`) and GAWD's
//! admin audits (`gawd.`) — are reached over the plane bus.
//!
//! The socket has no caller working directory: workspace-scoped commands act
//! on the server workspace, and every path argument must be absolute.
//! Secrets never cross: setting keys and verifying audit evidence against
//! signing secrets stay local to the CLI (see `tests/socket_parity.rs`).

use std::path::{Path, PathBuf};

use crate::susi_core::broker::{IpcBroker, PermissionScope};
use crate::susi_core::mac_policy::{MacPolicy, PrivacyMode};
use crate::susi_core::plane_bus::{daemon, gawd, gemi};
use crate::susi_error::{EaiError, EaiResult};

use super::args;
use super::forward::{forwarded_tools, policy_workspace, pretty, required_str};
use super::CoreTools;

/// `susi os capabilities invoke`'s default `--timeout-ms`.
const CAPABILITY_TIMEOUT_MS: u64 = 30_000;

/// An optional string argument (a non-string value is refused).
fn optional_str<'a>(arg: &'a serde_json::Value, key: &str) -> EaiResult<Option<&'a str>> {
    match arg.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(Some)
            .ok_or_else(|| EaiError::config(format!("{key} must be a string"))),
    }
}

fn optional_u64(arg: &serde_json::Value, key: &str) -> EaiResult<Option<u64>> {
    match arg.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| EaiError::config(format!("{key} must be a non-negative integer"))),
    }
}

fn optional_bool(arg: &serde_json::Value, key: &str) -> EaiResult<bool> {
    match arg.get(key) {
        None | Some(serde_json::Value::Null) => Ok(false),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| EaiError::config(format!("{key} must be a boolean"))),
    }
}

/// A required absolute path argument: the socket has no caller cwd.
fn absolute_path(arg: &serde_json::Value, key: &str) -> EaiResult<PathBuf> {
    let path = PathBuf::from(required_str(arg, key)?);
    if !path.is_absolute() {
        return Err(EaiError::config(format!(
            "{key} must be an absolute path (the socket has no caller working directory)"
        )));
    }
    Ok(path)
}

fn audit_log(workspace: &Path) -> PathBuf {
    susi_paths::workspace_state_dir(workspace).join("audit.log")
}

fn service_control(action: &str, arg: &serde_json::Value) -> EaiResult<String> {
    let report =
        daemon::services_control(action, required_str(arg, "name")?).map_err(EaiError::config)?;
    pretty(&report)
}

fn workspace_report(
    arg: &serde_json::Value,
    server: &Path,
    write: bool,
    run: fn(&Path) -> Result<serde_json::Value, String>,
) -> EaiResult<String> {
    let workspace = policy_workspace(arg, "workspace", server, write)?;
    let report = run(&workspace).map_err(EaiError::internal)?;
    pretty(&report)
}

impl CoreTools {
    // ── 8. Context and IPC ─────────────────────────────────────────────

    pub fn ipc_pending(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let grantor = optional_str(arg, "grantor")?;
        let pending = IpcBroker::global().pending_requests(grantor);
        pretty(&serde_json::json!({ "pending": pending }))
    }

    pub fn ipc_check(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let grantee = required_str(arg, "grantee")?;
        let scope =
            PermissionScope::new(required_str(arg, "resource")?, required_str(arg, "action")?);
        let permitted = IpcBroker::global().is_permitted(grantee, &scope);
        pretty(&serde_json::json!({
            "grantee": grantee,
            "resource": scope.resource,
            "action": scope.action,
            "permitted": permitted,
        }))
    }

    pub fn ipc_revoke(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let grantee = required_str(arg, "grantee")?;
        let scope =
            PermissionScope::new(required_str(arg, "resource")?, required_str(arg, "action")?);
        let removed = IpcBroker::global().revoke(grantee, &scope);
        pretty(&serde_json::json!({ "revoked": removed.is_some() }))
    }

    pub fn ipc_list(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let grantee = required_str(arg, "grantee")?;
        let grants = IpcBroker::global().grants_for(grantee);
        pretty(&serde_json::json!({ "grantee": grantee, "grants": grants }))
    }

    pub fn privacy_mode(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let mode = required_str(arg, "mode")?;
        // `PrivacyMode::parse` maps anything unknown to balanced; over the
        // socket a typo must not silently relax a stricter mode.
        if PrivacyMode::parse_requested(mode).is_none() {
            return Err(EaiError::config(format!(
                "unknown privacy mode `{mode}` (expected balanced, local_only, host_only or open)"
            )));
        }
        let report = daemon::privacy_mode(mode).map_err(EaiError::config)?;
        pretty(&report)
    }

    pub fn privacy_grant(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let subject = optional_str(arg, "subject")?.unwrap_or("susi");
        let action = required_str(arg, "action")?;
        let resource = optional_str(arg, "resource")?.unwrap_or("*");
        let ttl_secs = optional_u64(arg, "ttl_secs")?;
        let token = MacPolicy::global().grant(subject, action, resource, ttl_secs)?;
        pretty(&token)
    }

    pub fn privacy_revoke(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let subject = optional_str(arg, "subject")?.unwrap_or("susi");
        let action = required_str(arg, "action")?;
        let resource = optional_str(arg, "resource")?.unwrap_or("*");
        let revoked = MacPolicy::global().revoke(subject, action, resource)?;
        pretty(&serde_json::json!({ "revoked": revoked }))
    }

    pub fn intent_list(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let bus = crate::susi_core::intent_bus::IntentBus::global();
        pretty(&serde_json::json!({
            "providers": bus.providers(),
            "needs": bus.needs(),
        }))
    }

    pub fn ambient_start(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        workspace_report(arg, workspace, true, daemon::ambient_start)
    }

    pub fn tx_list(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        // The CLI runs this in its working directory; the socket
        // takes an optional, MAC-policy-admitted `workspace`.
        let workspace = &super::forward::policy_workspace(arg, "workspace", workspace, false)?;
        let txm = crate::susi_core::agent_tx::TxManager::global();
        // Transactions begun by another process exist only in the durable
        // journal until hydrated.
        txm.hydrate(workspace);
        pretty(&serde_json::json!({ "open": txm.list_open() }))
    }

    pub fn context_graph_sample_os(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let user = optional_str(arg, "user")?;
        // The graph is the one substrate-home log the CLI and daemon share;
        // the workspace is only the records' tag, nothing under it is read.
        let target = match optional_str(arg, "workspace")? {
            Some(_) => absolute_path(arg, "workspace")?,
            None => workspace.to_path_buf(),
        };
        let report = daemon::context_sample_os(&target, user).map_err(EaiError::internal)?;
        pretty(&report)
    }

    // ── 5. Assurance ───────────────────────────────────────────────────

    pub fn audit_verify(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        // The CLI runs this in its working directory; the socket
        // takes an optional, MAC-policy-admitted `workspace`.
        let workspace = &super::forward::policy_workspace(arg, "workspace", workspace, false)?;
        let since = optional_u64(arg, "since")?;
        let file = audit_log(workspace);
        let count = crate::susi_sandbox::audit_chain::verify_chain_since(&file, since)
            .map_err(|e| EaiError::governance(format!("Audit chain verification failed: {e}")))?;
        pretty(&serde_json::json!({
            "verified": true,
            "records": count,
            "since": since,
            "audit_log": file.display().to_string(),
        }))
    }

    pub fn audit_query(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        // The CLI runs this in its working directory; the socket
        // takes an optional, MAC-policy-admitted `workspace`.
        let workspace = &super::forward::policy_workspace(arg, "workspace", workspace, false)?;
        let text = |key: &str| optional_str(arg, key).map(|v| v.map(str::to_string));
        let query = crate::susi_sandbox::audit_chain::AuditQuery {
            actor: text("actor")?,
            mission: text("mission")?,
            kind: text("kind")?,
            since: optional_u64(arg, "since")?,
            until: optional_u64(arg, "until")?,
        };
        let records = crate::susi_sandbox::audit_chain::query_chain(&audit_log(workspace), &query)
            .map_err(|e| EaiError::governance(format!("Audit query failed: {e}")))?;
        pretty(&records)
    }

    pub fn compliance_audit(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        // The CLI runs this in its working directory; the socket
        // takes an optional, MAC-policy-admitted `workspace`.
        let workspace = &super::forward::policy_workspace(arg, "workspace", workspace, false)?;
        gawd::audit_compliance(workspace).map_err(EaiError::governance)
    }

    pub fn audit_evidence_export(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let input = absolute_path(arg, "input")?;
        let output = absolute_path(arg, "output")?;
        let report = gawd::audit_evidence_export(&input, &output).map_err(EaiError::config)?;
        pretty(&report)
    }

    pub fn substrate_report(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        // The CLI runs this in its working directory; the socket
        // takes an optional, MAC-policy-admitted `workspace`.
        let workspace = &super::forward::policy_workspace(arg, "workspace", workspace, false)?;
        gawd::substrate_report(workspace).map_err(EaiError::internal)
    }

    pub fn substrate_status(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        workspace_report(arg, workspace, false, daemon::substrate_status)
    }

    pub fn crown_verify(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        workspace_report(arg, workspace, false, daemon::crown_verify)
    }

    pub fn clean(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        pretty(&crate::susi_core::substrate_hygiene::clean())
    }

    // ── 4. Onboarding ──────────────────────────────────────────────────

    pub fn setup_plan(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let home = susi_paths::SusiDirs::config_dir();
        pretty(&crate::susi_config::setup_workflow::SetupPlan::detect(
            &home,
        ))
    }

    pub fn setup_apply(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        use crate::susi_config::setup_workflow::{ConsentKind, SetupPlan};
        let cloud = optional_bool(arg, "cloud")?;
        let home = susi_paths::SusiDirs::config_dir();
        let mut plan = SetupPlan::detect(&home);
        if cloud {
            plan.grant_consent(ConsentKind::CloudApiKey);
        }
        let summary = plan.apply(&home).map_err(EaiError::config)?;
        pretty(&serde_json::json!({
            "hardware": plan.hardware,
            "engines": plan.engines,
            "keys_present": plan.keys_present,
            "agents": plan.agents,
            "summary": summary,
        }))
    }

    pub fn ecosystem_scan(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let cached = optional_bool(arg, "cached")?;
        let report = daemon::ecosystem_scan(cached).map_err(EaiError::config)?;
        pretty(&report)
    }

    pub fn ecosystem_start(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let id = required_str(arg, "id")?;
        let message = gemi::ecosystem_start(id).map_err(EaiError::config)?;
        pretty(&serde_json::json!({ "id": id, "message": message }))
    }

    pub fn ecosystem_kb(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let mut query = serde_json::Map::new();
        for key in [
            "action",
            "id",
            "query",
            "kind",
            "capability",
            "vendor",
            "dir",
        ] {
            if let Some(value) = optional_str(arg, key)? {
                query.insert(key.to_string(), value.into());
            }
        }
        let report =
            gemi::ecosystem_kb(serde_json::Value::Object(query)).map_err(EaiError::config)?;
        pretty(&report)
    }

    pub fn ecosystem_explain(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let id = required_str(arg, "id")?;
        let dir = match optional_str(arg, "dir")? {
            Some(_) => Some(absolute_path(arg, "dir")?),
            None => None,
        };
        let report = gemi::ecosystem_explain(id, dir.as_deref()).map_err(EaiError::config)?;
        pretty(&report)
    }

    // ── 9. OS ──────────────────────────────────────────────────────────

    pub fn os_provision(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let action = optional_str(arg, "action")?.unwrap_or("probe");
        let mut payload = serde_json::json!({ "action": action });
        if let Some(kind) = optional_str(arg, "kind")? {
            payload["kind"] = kind.into();
        }
        if optional_str(arg, "manifest")?.is_some() {
            payload["manifest"] = absolute_path(arg, "manifest")?.display().to_string().into();
        }
        let report = daemon::os_provision(payload).map_err(EaiError::config)?;
        pretty(&report)
    }

    pub fn os_capabilities(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        // The CLI runs this in its working directory; the socket
        // takes an optional, MAC-policy-admitted `workspace`.
        let workspace = &super::forward::policy_workspace(arg, "workspace", workspace, true)?;
        let registry = crate::susi_core::registry::CapabilityRegistry::global();
        match optional_str(arg, "action")?.unwrap_or("discover") {
            "discover" => pretty(&registry.typed_bus().discover()),
            action @ ("invoke" | "delegate") => {
                let id = required_str(arg, "id")?;
                let permission = required_str(arg, "permission")?;
                let subject = optional_str(arg, "subject")?.unwrap_or("operator");
                let args = match arg.get("args") {
                    None | Some(serde_json::Value::Null) => serde_json::json!({}),
                    Some(value @ serde_json::Value::Object(_)) => value.clone(),
                    Some(_) => return Err(EaiError::config("args must be an object")),
                };
                let request = crate::susi_core::capability_bus::CapabilityRequest::new(
                    id,
                    subject,
                    [permission.to_string()],
                    optional_u64(arg, "timeout_ms")?.unwrap_or(CAPABILITY_TIMEOUT_MS),
                    optional_u64(arg, "budget_micros")?.unwrap_or(u64::MAX),
                );
                let receipt = registry
                    .serve_nested(action, arg.get("delegation"), &request, &args, workspace)
                    .map_err(|e| EaiError::governance(format!("{e:?}")))?;
                pretty(&receipt)
            }
            other => Err(EaiError::config(format!(
                "unknown capabilities action `{other}` (expected discover, invoke or delegate)"
            ))),
        }
    }

    forwarded_tools! {
        services_start(services_start_tool_attr, args::typed::<args::ServiceName>()),
        "Clear an operator stop so the supervisor brings a leaf service back up, as `susi services start`. Args: name",
        |arg, _workspace| service_control("start", arg);

        services_stop(services_stop_tool_attr, args::typed::<args::ServiceName>()),
        "Stop a supervised leaf service and hold it down until services_start, as `susi services stop`. Args: name",
        |arg, _workspace| service_control("stop", arg);

        services_restart(services_restart_tool_attr, args::typed::<args::ServiceName>()),
        "Terminate a supervised leaf service so the daemon respawns it (works on Windows and Unix), as `susi services restart`. Args: name",
        |arg, _workspace| service_control("restart", arg);
    }

    pub fn commits_audit(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        pretty(&crate::susi_core::commit_log_report::audit_report())
    }

    pub fn commits_replay(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        pretty(&crate::susi_core::commit_log_report::replay_report())
    }

    pub fn commits_show(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let report = crate::susi_core::commit_log_report::show_report(required_str(arg, "epoch")?)
            .map_err(EaiError::config)?;
        pretty(&report)
    }

    pub fn commits_sync(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        pretty(&crate::susi_core::commit_log_report::sync_report())
    }

    pub fn commits_compact(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let report =
            crate::susi_core::commit_log_report::compact_report().map_err(EaiError::internal)?;
        pretty(&report)
    }

    pub fn peers_probe(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let host = required_str(arg, "host")?;
        let port = match optional_u64(arg, "port")? {
            None => None,
            Some(n) => {
                Some(u16::try_from(n).map_err(|_| EaiError::config("port must be 0-65535"))?)
            }
        };
        let report =
            crate::susi_core::peer_probe::probe_report(host, port).map_err(EaiError::network)?;
        pretty(&report)
    }

    pub fn peers_list(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let report = crate::susi_core::peer_roster::list_report().map_err(EaiError::config)?;
        pretty(&report)
    }

    pub fn peers_status(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let rows = crate::susi_core::peer_roster::status_rows().map_err(EaiError::config)?;
        pretty(&rows)
    }
}

#[cfg(test)]
#[path = "../tests/host_ops.rs"]
mod tests;
