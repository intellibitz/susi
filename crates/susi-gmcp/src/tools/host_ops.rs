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
use super::CoreTools;

/// `susi os capabilities invoke`'s default `--timeout-ms`.
const CAPABILITY_TIMEOUT_MS: u64 = 30_000;

fn pretty(value: &serde_json::Value) -> EaiResult<String> {
    serde_json::to_string_pretty(value)
        .map_err(|error| EaiError::internal(format!("encode tool result: {error}")))
}

fn encode<T: serde::Serialize>(value: &T) -> EaiResult<String> {
    serde_json::to_string_pretty(value)
        .map_err(|error| EaiError::internal(format!("encode tool result: {error}")))
}

/// A required, non-empty string argument.
fn required_str<'a>(arg: &'a serde_json::Value, key: &str) -> EaiResult<&'a str> {
    arg.get(key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| EaiError::config(format!("{key} is required")))
}

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
    workspace.join(".susi").join("audit.log")
}

impl CoreTools {
    // ── 8. Context and IPC ─────────────────────────────────────────────

    crate::tool_def!(ipc_pending_tool_attr {
        name: "ipc_pending",
        description: "Pending inter-app permission requests, as `susi broker pending`. Args: optional grantor",
        input_schema: args::typed::<args::IpcPending>(),
    });
    pub fn ipc_pending(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let grantor = optional_str(arg, "grantor")?;
        let pending = IpcBroker::global().pending_requests(grantor);
        encode(&serde_json::json!({ "pending": pending }))
    }

    crate::tool_def!(ipc_check_tool_attr {
        name: "ipc_check",
        description: "Whether an identity holds an inter-app permission scope, as `susi broker check`. Args: grantee, resource, action",
        input_schema: args::typed::<args::IpcScope>(),
    });
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

    crate::tool_def!(ipc_revoke_tool_attr {
        name: "ipc_revoke",
        description: "Revoke an inter-app permission scope from an identity, as `susi broker revoke`. Args: grantee, resource, action",
        input_schema: args::typed::<args::IpcScope>(),
    });
    pub fn ipc_revoke(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let grantee = required_str(arg, "grantee")?;
        let scope =
            PermissionScope::new(required_str(arg, "resource")?, required_str(arg, "action")?);
        let removed = IpcBroker::global().revoke(grantee, &scope);
        pretty(&serde_json::json!({ "revoked": removed.is_some() }))
    }

    crate::tool_def!(ipc_list_tool_attr {
        name: "ipc_list",
        description:
            "Inter-app permission grants held by an identity, as `susi broker list`. Args: grantee",
        input_schema: args::typed::<args::IpcList>(),
    });
    pub fn ipc_list(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let grantee = required_str(arg, "grantee")?;
        let grants = IpcBroker::global().grants_for(grantee);
        encode(&serde_json::json!({ "grantee": grantee, "grants": grants }))
    }

    crate::tool_def!(privacy_mode_tool_attr {
        name: "privacy_mode",
        description: "Set the privacy mode (persisted; local_only keeps traffic on this machine and its local network, host_only on this machine alone (loopback; no LAN, discovery, gossip or off-host peers); both pin inference routing to local), as `susi privacy mode`. Args: mode (balanced, local_only, host_only, open)",
        input_schema: args::typed::<args::PrivacyMode>(),
    });
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

    crate::tool_def!(privacy_grant_tool_attr {
        name: "privacy_grant",
        description: "Grant a cryptographic capability token, as `susi privacy grant`. Args: action, optional subject (default susi), resource (default *), ttl_secs",
        input_schema: args::typed::<args::PrivacyGrant>(),
    });
    pub fn privacy_grant(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let subject = optional_str(arg, "subject")?.unwrap_or("susi");
        let action = required_str(arg, "action")?;
        let resource = optional_str(arg, "resource")?.unwrap_or("*");
        let ttl_secs = optional_u64(arg, "ttl_secs")?;
        let token = MacPolicy::global().grant(subject, action, resource, ttl_secs)?;
        encode(&token)
    }

    crate::tool_def!(privacy_revoke_tool_attr {
        name: "privacy_revoke",
        description: "Revoke a capability grant, as `susi privacy revoke`. Args: action, optional subject (default susi), resource (default *)",
        input_schema: args::typed::<args::PrivacyRevoke>(),
    });
    pub fn privacy_revoke(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let subject = optional_str(arg, "subject")?.unwrap_or("susi");
        let action = required_str(arg, "action")?;
        let resource = optional_str(arg, "resource")?.unwrap_or("*");
        let revoked = MacPolicy::global().revoke(subject, action, resource)?;
        pretty(&serde_json::json!({ "revoked": revoked }))
    }

    crate::tool_def!(intent_list_tool_attr {
        name: "intent_list",
        description: "Active intent-bus providers and needs, as `susi intent list`",
        input_schema: args::no_args(),
    });
    pub fn intent_list(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let bus = crate::susi_core::intent_bus::IntentBus::global();
        encode(&serde_json::json!({
            "providers": bus.providers(),
            "needs": bus.needs(),
        }))
    }

    crate::tool_def!(ambient_start_tool_attr {
        name: "ambient_start",
        description: "Start the background ambient indexer for the server workspace, as `susi ambient start`; reports started, already_running or refused_internal_path. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        input_schema: args::typed::<args::WorkspaceView>(),
    });
    pub fn ambient_start(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        // The CLI runs this in its working directory; the socket
        // takes an optional, MAC-policy-admitted `workspace`.
        let workspace = &super::forward::policy_workspace(arg, "workspace", workspace, true)?;
        let report = daemon::ambient_start(workspace).map_err(EaiError::internal)?;
        pretty(&report)
    }

    crate::tool_def!(tx_list_tool_attr {
        name: "tx_list",
        description: "Open multi-agent transactions in the server workspace, as `susi tx list`. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        input_schema: args::typed::<args::WorkspaceView>(),
    });
    pub fn tx_list(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        // The CLI runs this in its working directory; the socket
        // takes an optional, MAC-policy-admitted `workspace`.
        let workspace = &super::forward::policy_workspace(arg, "workspace", workspace, false)?;
        let txm = crate::susi_core::agent_tx::TxManager::global();
        // Transactions begun by another process exist only in the durable
        // journal until hydrated.
        txm.hydrate(workspace);
        encode(&serde_json::json!({ "open": txm.list_open() }))
    }

    crate::tool_def!(context_graph_sample_os_tool_attr {
        name: "context_graph_sample_os",
        description: "Sample OS context (process names; active window on Linux) into the Universal Context Graph, as `susi context-graph sample-os`. Args: optional user, optional absolute workspace to tag the records with (default: the server workspace)",
        input_schema: args::typed::<args::ContextSampleOs>(),
    });
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

    crate::tool_def!(audit_verify_tool_attr {
        name: "audit_verify",
        description: "Verify every record of the server workspace's tamper-evident audit chain, as `susi audit verify`. Args: optional since (Unix seconds; counts entries at or after it, the full chain is checked). Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        input_schema: args::typed::<args::AuditVerify>(),
    });
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

    crate::tool_def!(audit_query_tool_attr {
        name: "audit_query",
        description: "Redacted action history from the server workspace's audit chain, as `susi audit query --json`. Args: optional actor, mission, kind, since, until. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        input_schema: args::typed::<args::AuditQuery>(),
    });
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
        encode(&records)
    }

    crate::tool_def!(compliance_audit_tool_attr {
        name: "compliance_audit",
        description: "The deterministic compliance audit of the server workspace, as `susi audit` and `susi admin audit`. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        input_schema: args::typed::<args::WorkspaceView>(),
    });
    pub fn compliance_audit(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        // The CLI runs this in its working directory; the socket
        // takes an optional, MAC-policy-admitted `workspace`.
        let workspace = &super::forward::policy_workspace(arg, "workspace", workspace, false)?;
        gawd::audit_compliance(workspace).map_err(EaiError::governance)
    }

    crate::tool_def!(audit_evidence_export_tool_attr {
        name: "audit_evidence_export",
        description: "Wrap redacted audit segments in the independently verifiable export format, as `susi admin audit-evidence export`. Args: input, output (absolute paths)",
        input_schema: args::typed::<args::AuditEvidenceExport>(),
    });
    pub fn audit_evidence_export(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let input = absolute_path(arg, "input")?;
        let output = absolute_path(arg, "output")?;
        let report = gawd::audit_evidence_export(&input, &output).map_err(EaiError::config)?;
        pretty(&report)
    }

    crate::tool_def!(substrate_report_tool_attr {
        name: "substrate_report",
        description: "The compiled-constants substrate report (version, model, axioms, topology), as `susi substrate-report`. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        input_schema: args::typed::<args::WorkspaceView>(),
    });
    pub fn substrate_report(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        // The CLI runs this in its working directory; the socket
        // takes an optional, MAC-policy-admitted `workspace`.
        let workspace = &super::forward::policy_workspace(arg, "workspace", workspace, false)?;
        gawd::substrate_report(workspace).map_err(EaiError::internal)
    }

    crate::tool_def!(substrate_status_tool_attr {
        name: "substrate_status",
        description: "Pluggable / Sandbox / Governance / Host-contract / Reflexes status: the same document as `susi substrate status`. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        input_schema: args::typed::<args::WorkspaceView>(),
    });
    pub fn substrate_status(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        // The CLI runs this in its working directory; the socket
        // takes an optional, MAC-policy-admitted `workspace`.
        let workspace = &super::forward::policy_workspace(arg, "workspace", workspace, false)?;
        let report = daemon::substrate_status(workspace).map_err(EaiError::internal)?;
        pretty(&report)
    }

    crate::tool_def!(crown_verify_tool_attr {
        name: "crown_verify",
        description: "Verify every Tier S USP: the same document as `susi crown verify|status` (all_critical_hold says whether verify would pass). Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        input_schema: args::typed::<args::WorkspaceView>(),
    });
    pub fn crown_verify(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        // The CLI runs this in its working directory; the socket
        // takes an optional, MAC-policy-admitted `workspace`.
        let workspace = &super::forward::policy_workspace(arg, "workspace", workspace, false)?;
        let report = daemon::crown_verify(workspace).map_err(EaiError::internal)?;
        pretty(&report)
    }

    crate::tool_def!(clean_tool_attr {
        name: "clean",
        description: "Remove SUSI-owned residue (never host or other apps' caches) and report bytes freed, as `susi os clean` / `susi os-clean`",
        input_schema: args::no_args(),
    });
    pub fn clean(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        encode(&crate::susi_core::substrate_hygiene::clean())
    }

    // ── 4. Onboarding ──────────────────────────────────────────────────

    crate::tool_def!(setup_plan_tool_attr {
        name: "setup_plan",
        description: "Detect hardware, engines, key presence (names only) and agents, and return the zero-config setup plan: the same document as `susi setup plan --json`",
        input_schema: args::no_args(),
    });
    pub fn setup_plan(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let home = susi_paths::SusiDirs::config_dir();
        encode(&crate::susi_config::setup_workflow::SetupPlan::detect(
            &home,
        ))
    }

    crate::tool_def!(setup_apply_tool_attr {
        name: "setup_apply",
        description: "Apply the zero-config setup plan (existing settings preserved): the same document as `susi setup apply --json`. Args: optional cloud (grant cloud API key consent)",
        input_schema: args::typed::<args::SetupApply>(),
    });
    pub fn setup_apply(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        use crate::susi_config::setup_workflow::{ConsentKind, SetupPlan};
        let cloud = optional_bool(arg, "cloud")?;
        let home = susi_paths::SusiDirs::config_dir();
        let mut plan = SetupPlan::detect(&home);
        if cloud {
            plan.grant_consent(ConsentKind::CloudApiKey);
        }
        let summary = plan.apply(&home).map_err(EaiError::config)?;
        encode(&serde_json::json!({
            "hardware": plan.hardware,
            "engines": plan.engines,
            "keys_present": plan.keys_present,
            "agents": plan.agents,
            "summary": summary,
        }))
    }

    crate::tool_def!(ecosystem_scan_tool_attr {
        name: "ecosystem_scan",
        description: "Scan hardware and installed local AI engines, runtimes and model stores now, as `susi ecosystem scan`; with cached=true return the last persisted scan, as `susi ecosystem status`. Args: optional cached",
        input_schema: args::typed::<args::EcosystemScan>(),
    });
    pub fn ecosystem_scan(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let cached = optional_bool(arg, "cached")?;
        let report = daemon::ecosystem_scan(cached).map_err(EaiError::config)?;
        pretty(&report)
    }

    crate::tool_def!(ecosystem_start_tool_attr {
        name: "ecosystem_start",
        description: "Start an installed local AI engine that needs no further input (e.g. ollama, local-ai), as `susi ecosystem start`. Args: id",
        input_schema: args::typed::<args::EcosystemStart>(),
    });
    pub fn ecosystem_start(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let id = required_str(arg, "id")?;
        let message = gemi::ecosystem_start(id).map_err(EaiError::config)?;
        pretty(&serde_json::json!({ "id": id, "message": message }))
    }

    crate::tool_def!(ecosystem_kb_tool_attr {
        name: "ecosystem_kb",
        description: "Query the cloud-AI-ecosystem knowledge base, as `susi ecosystem kb list|show|search|check`. Args: optional action (default list), id, query, kind, capability, vendor, dir (absolute)",
        input_schema: args::typed::<args::EcosystemKb>(),
    });
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

    crate::tool_def!(ecosystem_explain_tool_attr {
        name: "ecosystem_explain",
        description: "Explain a component, standard or protocol end to end (versions, implementers, capabilities, susi profile), as `susi ecosystem explain`. Args: id, optional dir (absolute)",
        input_schema: args::typed::<args::EcosystemExplain>(),
    });
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

    crate::tool_def!(os_provision_tool_attr {
        name: "os_provision",
        description: "Explicit cloud provisioning, as `susi os provision probe|list|apply --json`. Args: optional action (default probe), kind (list/apply), manifest (apply; absolute path)",
        input_schema: args::typed::<args::OsProvision>(),
    });
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

    crate::tool_def!(os_capabilities_tool_attr {
        name: "os_capabilities",
        description: "The typed capability bus, as `susi os capabilities discover|invoke|delegate --json`. Args: optional action (default discover); invoke: id, permission, optional subject (default operator), args, timeout_ms (default 30000), budget_micros (default unlimited), delegation (a host-signed susi-delegation/v1 envelope: the call nests under it and cannot exceed it); delegate: the same fields, returns a host-signed envelope for subject to exercise id under delegation (or a fresh root). Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        input_schema: args::typed::<args::OsCapabilities>(),
    });
    pub fn os_capabilities(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        // The CLI runs this in its working directory; the socket
        // takes an optional, MAC-policy-admitted `workspace`.
        let workspace = &super::forward::policy_workspace(arg, "workspace", workspace, true)?;
        let registry = crate::susi_core::registry::CapabilityRegistry::global();
        match optional_str(arg, "action")?.unwrap_or("discover") {
            "discover" => encode(&registry.typed_bus().discover()),
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
                encode(&receipt)
            }
            other => Err(EaiError::config(format!(
                "unknown capabilities action `{other}` (expected discover, invoke or delegate)"
            ))),
        }
    }

    crate::tool_def!(services_start_tool_attr {
        name: "services_start",
        description: "Clear an operator stop so the supervisor brings a leaf service back up, as `susi services start`. Args: name",
        input_schema: args::typed::<args::ServiceName>(),
    });
    pub fn services_start(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let report = daemon::services_control("start", required_str(arg, "name")?)
            .map_err(EaiError::config)?;
        pretty(&report)
    }

    crate::tool_def!(services_stop_tool_attr {
        name: "services_stop",
        description: "Stop a supervised leaf service and hold it down until services_start, as `susi services stop`. Args: name",
        input_schema: args::typed::<args::ServiceName>(),
    });
    pub fn services_stop(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let report = daemon::services_control("stop", required_str(arg, "name")?)
            .map_err(EaiError::config)?;
        pretty(&report)
    }

    crate::tool_def!(services_restart_tool_attr {
        name: "services_restart",
        description: "Terminate a supervised leaf service so the daemon respawns it (works on Windows and Unix), as `susi services restart`. Args: name",
        input_schema: args::typed::<args::ServiceName>(),
    });
    pub fn services_restart(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let report = daemon::services_control("restart", required_str(arg, "name")?)
            .map_err(EaiError::config)?;
        pretty(&report)
    }

    crate::tool_def!(commits_audit_tool_attr {
        name: "commits_audit",
        description: "Whole-ledger consistency check of the replicated commit log (signatures, gaps, equivocation, non-leader commits): the same document as `susi commits audit --json`",
        input_schema: args::no_args(),
    });
    pub fn commits_audit(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        pretty(&crate::susi_core::commit_log_report::audit_report())
    }

    crate::tool_def!(commits_replay_tool_attr {
        name: "commits_replay",
        description: "The consensus view the commit ledger replays to (term, leader, roster, high-water marks): the same document as `susi commits replay --json`",
        input_schema: args::no_args(),
    });
    pub fn commits_replay(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        pretty(&crate::susi_core::commit_log_report::replay_report())
    }

    crate::tool_def!(commits_show_tool_attr {
        name: "commits_show",
        description: "One commit record (live or archived) by unique epoch prefix, with its signature verdicts: the same document as `susi commits show --json`. Args: epoch",
        input_schema: args::typed::<args::CommitsShow>(),
    });
    pub fn commits_show(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let report = crate::susi_core::commit_log_report::show_report(required_str(arg, "epoch")?)
            .map_err(EaiError::config)?;
        pretty(&report)
    }

    crate::tool_def!(commits_sync_tool_attr {
        name: "commits_sync",
        description: "Bidirectional commit-ledger anti-entropy with every verified remote peer, as `susi commits sync`; returns per-peer and total counts",
        input_schema: args::no_args(),
    });
    pub fn commits_sync(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        pretty(&crate::susi_core::commit_log_report::sync_report())
    }

    crate::tool_def!(commits_compact_tool_attr {
        name: "commits_compact",
        description: "Fold the commit ledger into a snapshot and archive pre-snapshot records, as `susi commits compact`",
        input_schema: args::no_args(),
    });
    pub fn commits_compact(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let report =
            crate::susi_core::commit_log_report::compact_report().map_err(EaiError::internal)?;
        pretty(&report)
    }

    crate::tool_def!(peers_probe_tool_attr {
        name: "peers_probe",
        description: "Verify a host cryptographically (signed ping/pong, then its MCP endpoint) without admitting it, as `susi peers probe`. Args: host (optionally host:port), optional port",
        input_schema: args::typed::<args::PeersProbe>(),
    });
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

    crate::tool_def!(peers_list_tool_attr {
        name: "peers_list",
        description: "This node's identity, eviction state, elected leader, and every rostered and banned cluster member: the same document as `susi peers list --json`",
        input_schema: args::no_args(),
    });
    pub fn peers_list(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let report = crate::susi_core::peer_roster::list_report().map_err(EaiError::config)?;
        pretty(&report)
    }

    crate::tool_def!(peers_status_tool_attr {
        name: "peers_status",
        description: "Every rostered member's cluster_status (unreachable members as error rows) plus this node's own, sorted by node: the same rows as `susi peers status --json`",
        input_schema: args::no_args(),
    });
    pub fn peers_status(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let rows = crate::susi_core::peer_roster::status_rows().map_err(EaiError::config)?;
        encode(&rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::susi_core::plane_bus::{topics, PlaneBus, PlaneHandler};
    use serde_json::json;
    use std::sync::Arc;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "susi-host-ops-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Echoes topic and payload, so a test sees exactly what a tool sent
    /// over the plane bus; `{"refuse": true}` payloads come back as errors.
    struct Echo;
    impl PlaneHandler for Echo {
        fn handle(
            &self,
            topic: &str,
            payload: serde_json::Value,
        ) -> Result<serde_json::Value, String> {
            if payload["name"] == "refused" {
                return Err("refused by plane".to_string());
            }
            match topic {
                topics::GAWD_AUDIT_COMPLIANCE | topics::GAWD_SUBSTRATE_REPORT => {
                    Ok(json!({ "text": format!("{topic} {}", payload["workspace"]) }))
                }
                topics::GEMI_ECOSYSTEM_START => Ok(json!({ "message": "started" })),
                _ => Ok(json!({ "topic": topic, "payload": payload })),
            }
        }
    }

    fn wire_echo() {
        let bus = PlaneBus::global();
        for topic in [
            topics::DAEMON_PRIVACY_MODE,
            topics::DAEMON_AMBIENT_START,
            topics::DAEMON_CONTEXT_SAMPLE_OS,
            topics::DAEMON_ECOSYSTEM_SCAN,
            topics::DAEMON_SUBSTRATE_STATUS,
            topics::DAEMON_CROWN_VERIFY,
            topics::DAEMON_OS_PROVISION,
            topics::DAEMON_SERVICES_CONTROL,
            topics::GEMI_ECOSYSTEM_START,
            topics::GEMI_ECOSYSTEM_KB,
            topics::GEMI_ECOSYSTEM_EXPLAIN,
            topics::GAWD_AUDIT_COMPLIANCE,
            topics::GAWD_AUDIT_EVIDENCE_EXPORT,
            topics::GAWD_SUBSTRATE_REPORT,
        ] {
            bus.register(topic, Arc::new(Echo));
        }
    }

    fn doc(result: EaiResult<String>) -> serde_json::Value {
        serde_json::from_str(&result.unwrap()).unwrap()
    }

    #[test]
    fn broker_tools_return_the_cli_documents() {
        // Read-only against the broker: a grant would persist into the
        // process bus directory, so only an unknown identity is queried.
        let ws = Path::new(".");
        let grantee = format!("host-ops-unknown-app-{}", std::process::id());
        let scope = json!({"grantee": grantee, "resource": "files", "action": "read"});
        let checked = doc(CoreTools::ipc_check(&scope, ws));
        assert_eq!(
            checked,
            json!({"grantee": grantee, "resource": "files", "action": "read", "permitted": false})
        );
        let listed = doc(CoreTools::ipc_list(&json!({"grantee": grantee}), ws));
        assert_eq!(listed, json!({"grantee": grantee, "grants": []}));
        assert_eq!(doc(CoreTools::ipc_revoke(&scope, ws))["revoked"], false);
        assert!(doc(CoreTools::ipc_pending(&json!({}), ws))["pending"].is_array());
        assert!(CoreTools::ipc_check(&json!({"grantee": grantee}), ws).is_err());
        assert!(CoreTools::ipc_list(&json!({}), ws).is_err());
        assert!(CoreTools::ipc_pending(&json!({"grantor": 3}), ws).is_err());
    }

    #[test]
    fn intent_and_tx_lists_are_served() {
        let ws = temp_dir("tx");
        let intents = doc(CoreTools::intent_list(&json!({}), &ws));
        assert!(intents["providers"].is_array() && intents["needs"].is_array());
        let open = doc(CoreTools::tx_list(&json!({}), &ws));
        assert!(open["open"].is_array());
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn privacy_mode_refuses_unknown_modes_before_reaching_the_daemon() {
        wire_echo();
        let ws = Path::new(".");
        let refused = CoreTools::privacy_mode(&json!({"mode": "lockdown"}), ws).unwrap_err();
        assert!(refused.to_string().contains("unknown privacy mode"));
        assert!(CoreTools::privacy_mode(&json!({}), ws).is_err());
        let sent = doc(CoreTools::privacy_mode(&json!({"mode": "local-only"}), ws));
        assert_eq!(sent["topic"], topics::DAEMON_PRIVACY_MODE);
        assert_eq!(sent["payload"]["mode"], "local-only");
        let balanced = doc(CoreTools::privacy_mode(&json!({"mode": "Balanced"}), ws));
        assert_eq!(balanced["payload"]["mode"], "Balanced");
        let host = doc(CoreTools::privacy_mode(&json!({"mode": "host-only"}), ws));
        assert_eq!(host["payload"]["mode"], "host-only");
        let typo = CoreTools::privacy_mode(&json!({"mode": "host_onyl"}), ws).unwrap_err();
        assert!(typo.to_string().contains("host_only"), "{typo}");
    }

    #[test]
    fn plane_backed_tools_forward_the_server_workspace_and_arguments() {
        wire_echo();
        let ws = temp_dir("plane");
        let ws_text = ws.display().to_string();
        let ambient = doc(CoreTools::ambient_start(&json!({}), &ws));
        assert_eq!(ambient["payload"]["workspace"], ws_text);
        let sample = doc(CoreTools::context_graph_sample_os(
            &json!({"user": "ceo"}),
            &ws,
        ));
        assert_eq!(sample["payload"]["user"], "ceo");
        assert_eq!(sample["payload"]["workspace"], ws_text);
        let tagged = doc(CoreTools::context_graph_sample_os(
            &json!({"workspace": ws_text}),
            Path::new("."),
        ));
        assert_eq!(tagged["payload"]["workspace"], ws_text);
        let relative = CoreTools::context_graph_sample_os(&json!({"workspace": "repo"}), &ws);
        assert!(relative.unwrap_err().to_string().contains("absolute"));
        let scan = doc(CoreTools::ecosystem_scan(&json!({"cached": true}), &ws));
        assert_eq!(scan["payload"]["cached"], true);
        assert!(CoreTools::ecosystem_scan(&json!({"cached": "yes"}), &ws).is_err());
        let status = doc(CoreTools::substrate_status(&json!({}), &ws));
        assert_eq!(status["topic"], topics::DAEMON_SUBSTRATE_STATUS);
        let crown = doc(CoreTools::crown_verify(&json!({}), &ws));
        assert_eq!(crown["payload"]["workspace"], ws_text);
        let audit = CoreTools::compliance_audit(&json!({}), &ws).unwrap();
        assert!(audit.starts_with(topics::GAWD_AUDIT_COMPLIANCE));
        let report = CoreTools::substrate_report(&json!({}), &ws).unwrap();
        assert!(report.starts_with(topics::GAWD_SUBSTRATE_REPORT));
        let started = doc(CoreTools::ecosystem_start(&json!({"id": "ollama"}), &ws));
        assert_eq!(started, json!({"id": "ollama", "message": "started"}));
        assert!(CoreTools::ecosystem_start(&json!({}), &ws).is_err());
        let kb = doc(CoreTools::ecosystem_kb(
            &json!({"action": "search", "query": "mcp", "ignored": 1}),
            &ws,
        ));
        assert_eq!(kb["payload"], json!({"action": "search", "query": "mcp"}));
        let explain = CoreTools::ecosystem_explain(&json!({"id": "mcp", "dir": "rel"}), &ws);
        assert!(explain.unwrap_err().to_string().contains("absolute"));
        let explained = doc(CoreTools::ecosystem_explain(&json!({"id": "mcp"}), &ws));
        assert_eq!(explained["payload"]["id"], "mcp");
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn os_tools_validate_paths_and_forward_requests() {
        wire_echo();
        let ws = Path::new(".");
        let probe = doc(CoreTools::os_provision(&json!({}), ws));
        assert_eq!(probe["payload"], json!({"action": "probe"}));
        let relative = CoreTools::os_provision(
            &json!({"action": "apply", "kind": "kubernetes", "manifest": "m.yaml"}),
            ws,
        );
        assert!(relative.unwrap_err().to_string().contains("absolute"));
        let manifest = std::env::temp_dir().join("m.yaml").display().to_string();
        let apply = doc(CoreTools::os_provision(
            &json!({"action": "apply", "kind": "kubernetes", "manifest": manifest}),
            ws,
        ));
        assert_eq!(apply["payload"]["manifest"], manifest);
        for (tool, action) in [
            (
                CoreTools::services_start as fn(&serde_json::Value, &Path) -> EaiResult<String>,
                "start",
            ),
            (CoreTools::services_stop, "stop"),
            (CoreTools::services_restart, "restart"),
        ] {
            let sent = doc(tool(&json!({"name": "susi-paths"}), ws));
            assert_eq!(
                sent["payload"],
                json!({"action": action, "name": "susi-paths"})
            );
            assert!(tool(&json!({}), ws).is_err());
            let refused = tool(&json!({"name": "refused"}), ws).unwrap_err();
            assert!(refused.to_string().contains("refused by plane"));
        }
        let export = CoreTools::audit_evidence_export(&json!({"input": "a", "output": "b"}), ws);
        assert!(export.unwrap_err().to_string().contains("absolute"));
        let input = std::env::temp_dir().join("in.json").display().to_string();
        let output = std::env::temp_dir().join("out.json").display().to_string();
        let exported = doc(CoreTools::audit_evidence_export(
            &json!({"input": input, "output": output}),
            ws,
        ));
        assert_eq!(exported["payload"]["output"], output);
    }

    #[test]
    fn os_capabilities_discovers_and_validates_invocations() {
        let ws = Path::new(".");
        assert!(doc(CoreTools::os_capabilities(&json!({}), ws)).is_array());
        assert!(CoreTools::os_capabilities(&json!({"action": "invoke"}), ws).is_err());
        assert!(CoreTools::os_capabilities(
            &json!({"action": "invoke", "id": "x", "permission": "p", "args": [1]}),
            ws
        )
        .is_err());
        assert!(CoreTools::os_capabilities(&json!({"action": "nope"}), ws).is_err());
        // An unknown capability is refused by the bus, not dispatched.
        assert!(CoreTools::os_capabilities(
            &json!({"action": "invoke", "id": "no-such-capability", "permission": "p"}),
            ws
        )
        .is_err());
    }

    #[test]
    fn audit_tools_read_the_server_workspace_chain() {
        let ws = temp_dir("audit");
        let empty = doc(CoreTools::audit_query(&json!({"actor": "nobody"}), &ws));
        assert_eq!(empty, json!([]));
        assert!(CoreTools::audit_query(&json!({"since": "yesterday"}), &ws).is_err());
        assert!(CoreTools::audit_verify(&json!({"since": -1}), &ws).is_err());
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn ledger_and_roster_tools_return_the_cli_documents_for_a_standalone_node() {
        let _lock = crate::susi_core::commit_log::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = temp_dir("roster-home");
        let mut env = susi_paths::test_env::EnvGuard::isolated();
        env.set("SUSI_HOME", &home);
        let ws = Path::new(".");
        let listed = doc(CoreTools::peers_list(&json!({}), ws));
        assert_eq!(
            listed,
            crate::susi_core::peer_roster::list_report().unwrap()
        );
        assert_eq!(listed["peers"], json!([]));
        let rows = doc(CoreTools::peers_status(&json!({}), ws));
        assert_eq!(rows.as_array().unwrap().len(), 1);
        assert_eq!(rows[0]["self"], true);
        let audit = doc(CoreTools::commits_audit(&json!({}), ws));
        assert_eq!(audit, crate::susi_core::commit_log_report::audit_report());
        assert_eq!(audit["ok"], true);
        let replay = doc(CoreTools::commits_replay(&json!({}), ws));
        assert_eq!(replay["decisions"], 0);
        let missing = CoreTools::commits_show(&json!({"epoch": "ffff"}), ws).unwrap_err();
        assert!(missing.to_string().contains("no commit record"));
        let sync = doc(CoreTools::commits_sync(&json!({}), ws));
        assert!(sync["note"].as_str().unwrap().contains("nothing to sync"));
        assert_eq!(
            doc(CoreTools::commits_compact(&json!({}), ws))["archived"],
            0
        );
        let cleaned = doc(CoreTools::clean(&json!({}), ws));
        assert_eq!(cleaned["removed_count"], 0);
        let plan = doc(CoreTools::setup_plan(&json!({}), ws));
        assert!(plan["keys_present"].is_array(), "{plan}");
        drop(env);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn commit_and_peer_tools_validate_arguments() {
        let ws = Path::new(".");
        assert!(CoreTools::commits_show(&json!({}), ws).is_err());
        assert!(CoreTools::peers_probe(&json!({}), ws).is_err());
        assert!(
            CoreTools::peers_probe(&json!({"host": "h", "port": 70000}), ws)
                .unwrap_err()
                .to_string()
                .contains("port")
        );
    }
}
