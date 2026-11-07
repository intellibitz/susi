//! Adapter lifecycle for external agents, agent frameworks, coding models
//! and leading MCP servers: what `susi agents|frameworks|models|mcp
//! doctor|setup|configure|reset`, `susi agents|frameworks retry` and
//! `susi mcp-add` do, and what the MCP `adapter_*`, `agents_retry`,
//! `frameworks_retry` and `mcp_add` tools do over the `daemon.adapters`
//! plane topic (zero-UI plan B4 group 7). One implementation for both: the
//! CLI calls these functions and prints what they return.
//!
//! Adapter overrides are host configuration (`~/.susi/<kind>/<id>.json`),
//! never instructions read from a repository; the workspace only roots the
//! durable task records `retry` reads and writes.

use serde_json::{Value, json};
use std::path::Path;
use susi_gawd::external::{Adapter, AgentManager, CatalogKind, RunRecord, RunStatus, catalog};
use susi_gemi::models::coding_models::{CodingModelManager, CodingModelOverride};
use susi_tools::{LeadingMcpManager, LeadingMcpOverride};

type R<T> = Result<T, String>;

fn msg(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn encode<T: serde::Serialize>(value: &T) -> R<Value> {
    serde_json::to_value(value).map_err(msg)
}

/// The adapter families with a doctor/setup/configure/reset lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdapterKind {
    /// `susi agents`: external coding/execution agents.
    Agents,
    /// `susi frameworks`: agent frameworks and engines.
    Frameworks,
    /// `susi models`: developer/agent-focused coding models.
    Models,
    /// `susi mcp`: the leading MCP tool servers.
    Mcp,
}

impl AdapterKind {
    /// Parse `agents`, `frameworks`, `models` or `mcp` (singular accepted).
    pub fn parse(name: &str) -> R<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "agents" | "agent" => Ok(Self::Agents),
            "frameworks" | "framework" => Ok(Self::Frameworks),
            "models" | "model" => Ok(Self::Models),
            "mcp" => Ok(Self::Mcp),
            other => Err(format!(
                "unknown adapter kind `{other}` (expected agents, frameworks, models or mcp)"
            )),
        }
    }

    /// The key each doctor row names its subject by, as the CLI prints it.
    pub fn id_key(self) -> &'static str {
        match self {
            Self::Agents => "agent",
            Self::Frameworks => "engine",
            Self::Models => "model",
            Self::Mcp => "server",
        }
    }

    fn catalog_kind(self) -> R<CatalogKind> {
        match self {
            Self::Agents => Ok(CatalogKind::Execution),
            Self::Frameworks => Ok(CatalogKind::Framework),
            Self::Models | Self::Mcp => Err(format!("{self:?} is not an agent catalog")),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Agents => "agents",
            Self::Frameworks => "frameworks",
            Self::Models => "coding models",
            Self::Mcp => "leading MCP servers",
        }
    }
}

/// The agent or framework manager for `workspace`.
pub fn manager(kind: AdapterKind, workspace: &Path) -> R<AgentManager> {
    AgentManager::for_kind(workspace, kind.catalog_kind()?).map_err(msg)
}

/// `susi <kind> doctor [id]`: one readiness row per adapter, in the order
/// the CLI prints them. Nothing is installed and no paid model is called.
pub fn doctor(kind: AdapterKind, id: Option<&str>, workspace: &Path) -> R<Vec<Value>> {
    let key = kind.id_key();
    match kind {
        AdapterKind::Agents | AdapterKind::Frameworks => {
            let manager = manager(kind, workspace)?;
            let catalog_kind = kind.catalog_kind()?;
            let entries = match id {
                Some(id) => vec![susi_gawd::external::definition(catalog_kind, id).map_err(msg)?],
                None => catalog(catalog_kind).map_err(msg)?,
            };
            Ok(entries
                .into_iter()
                .map(|entry| {
                    let result = manager.adapter(&entry.id).and_then(|a| a.preflight());
                    json!({
                        key: entry.id,
                        "prerequisites_present": result.is_ok(),
                        "detail": match result { Ok(s) => s, Err(e) => e.to_string() }
                    })
                })
                .collect())
        }
        AdapterKind::Models => {
            let manager = CodingModelManager::new().map_err(msg)?;
            let models = match id {
                Some(id) => vec![CodingModelManager::definition(id).map_err(msg)?],
                None => CodingModelManager::catalog().map_err(msg)?,
            };
            Ok(models
                .into_iter()
                .map(|m| {
                    let result = manager.preflight(&m.id);
                    json!({
                        key: m.id,
                        "prerequisites_present": result.is_ok(),
                        "detail": match result { Ok(s) => s, Err(e) => e.to_string() }
                    })
                })
                .collect())
        }
        AdapterKind::Mcp => {
            let manager = LeadingMcpManager::new(workspace).map_err(msg)?;
            let servers = match id {
                Some(id) => vec![LeadingMcpManager::definition(id).map_err(msg)?],
                None => LeadingMcpManager::catalog().map_err(msg)?,
            };
            Ok(servers
                .into_iter()
                .map(|srv| {
                    let result = manager.preflight(&srv.id);
                    json!({
                        key: srv.id,
                        "prerequisites_present": result.is_ok(),
                        "enabled": manager.is_enabled(&srv.id).unwrap_or(false),
                        "detail": match result { Ok(s) => s, Err(e) => e.to_string() }
                    })
                })
                .collect())
        }
    }
}

/// Whether every doctor row reports its prerequisites present (the CLI
/// fails with `one or more <kind> need setup` otherwise).
pub fn all_ready(rows: &[Value]) -> bool {
    rows.iter()
        .all(|r| r.get("prerequisites_present").and_then(Value::as_bool) == Some(true))
}

/// The CLI's failure line when [`all_ready`] is false.
pub fn needs_setup_message(kind: AdapterKind) -> String {
    format!("one or more {} need setup", kind.label())
}

const AGENT_SETUP: &str = "Install/authenticate the vendor CLI or set the cloud credential environment variable. Qwen requires qwen-agent and SUSI_QWEN_CONFIG. Run doctor, then run. No packages, subscriptions or credentials are provisioned implicitly.";
const FRAMEWORK_SETUP: &str = "Install the Python package (pip/uv). Set the adapter config_env to an absolute JSON file with either {\"script\":\"/abs/path.py\"} or {\"entrypoint\":\"module:callable\"}. The callable receives the prompt string. Model API keys stay in your environment. Run doctor, then run. No packages or credentials are provisioned implicitly.";
const MODEL_SETUP: &str = "Set the vendor key with `susi keys set <engine>` (or export api_key_env). API model ids drift — override with `susi models configure`. Run doctor, then optional probe (paid). Prefer pins routing. No subscriptions are provisioned implicitly.";

/// `susi <kind> setup <id>`: setup instructions and the effective adapter.
pub fn setup(kind: AdapterKind, id: &str, workspace: &Path) -> R<Value> {
    match kind {
        AdapterKind::Agents | AdapterKind::Frameworks => {
            let manager = manager(kind, workspace)?;
            let definition =
                susi_gawd::external::definition(kind.catalog_kind()?, id).map_err(msg)?;
            let instructions = if kind == AdapterKind::Agents {
                AGENT_SETUP
            } else {
                FRAMEWORK_SETUP
            };
            Ok(json!({
                kind.id_key(): definition.id,
                "documentation": definition.documentation,
                "adapter": manager.adapter(id).map_err(msg)?,
                "instructions": instructions,
            }))
        }
        AdapterKind::Models => {
            let manager = CodingModelManager::new().map_err(msg)?;
            let def = manager.effective(id).map_err(msg)?;
            Ok(json!({
                "model": def,
                "preferred": manager.preferred(),
                "instructions": MODEL_SETUP,
            }))
        }
        AdapterKind::Mcp => {
            let manager = LeadingMcpManager::new(workspace).map_err(msg)?;
            let def = manager.effective(id).map_err(msg)?;
            let credentials = if def.env_keys.is_empty() {
                "This peer declares no credential variables.".to_string()
            } else {
                format!(
                    "This peer requires authorization to supply {}.",
                    def.env_keys.join(", ")
                )
            };
            let instructions = format!(
                "Susi serves MCP natively with `susi mcp serve`. The optional `{id}` peer uses the configured launcher `{runner}`. {credentials} An agent can check readiness with `susi mcp doctor {id}` and register it with `susi mcp enable {id}`.",
                id = def.id,
                runner = def.runner,
            );
            Ok(json!({
                "server": def,
                "enabled": manager.is_enabled(&def.id).map_err(msg)?,
                "instructions": instructions,
            }))
        }
    }
}

/// `susi <kind> configure <id> <file>`: store a host override (`document`
/// is the file's JSON). Agent and framework adapters go through the
/// verified adapter upgrade, which refuses a candidate that fails it.
pub fn configure(kind: AdapterKind, id: &str, document: Value, workspace: &Path) -> R<Value> {
    match kind {
        AdapterKind::Agents | AdapterKind::Frameworks => {
            let manager = manager(kind, workspace)?;
            let adapter: Adapter = serde_json::from_value(document)
                .map_err(|e| format!("invalid adapter configuration: {e}"))?;
            let verdict = susi_gawd::adapter_upgrade::upgrade_adapter(&manager, id, &adapter, 2)
                .map_err(msg)?;
            if verdict != susi_core::adapter_upgrade::SwitchVerdict::Switched {
                return Err(format!("adapter upgrade rejected: {verdict:?}"));
            }
            encode(&adapter)
        }
        AdapterKind::Models => {
            let over: CodingModelOverride = serde_json::from_value(document)
                .map_err(|e| format!("invalid coding-model override: {e}"))?;
            let manager = CodingModelManager::new().map_err(msg)?;
            encode(&manager.configure(id, &over).map_err(msg)?)
        }
        AdapterKind::Mcp => {
            let over: LeadingMcpOverride = serde_json::from_value(document)
                .map_err(|e| format!("invalid leading MCP override: {e}"))?;
            let manager = LeadingMcpManager::new(workspace).map_err(msg)?;
            encode(&manager.configure(id, &over).map_err(msg)?)
        }
    }
}

/// `susi <kind> reset <id>`: drop the host override; returns the bundled
/// adapter now in effect.
pub fn reset(kind: AdapterKind, id: &str, workspace: &Path) -> R<Value> {
    match kind {
        AdapterKind::Agents | AdapterKind::Frameworks => {
            let manager = manager(kind, workspace)?;
            manager.reset(id).map_err(msg)?;
            encode(&manager.adapter(id).map_err(msg)?)
        }
        AdapterKind::Models => {
            let manager = CodingModelManager::new().map_err(msg)?;
            encode(&manager.reset(id).map_err(msg)?)
        }
        AdapterKind::Mcp => {
            let manager = LeadingMcpManager::new(workspace).map_err(msg)?;
            encode(&manager.reset(id).map_err(msg)?)
        }
    }
}

/// The finished task `susi <agents|frameworks> retry <id>` re-runs: an
/// active or unknown task is refused, so a retry never duplicates work
/// still in flight. The retry is a new task with the original prompt.
pub fn retry_source(manager: &AgentManager, task_id: &str) -> R<RunRecord> {
    let old = manager.status(task_id).map_err(msg)?;
    if old.status.active() || old.status == RunStatus::Unknown {
        return Err(
            "resolve the existing task before retrying to avoid duplicate execution".to_string(),
        );
    }
    Ok(old)
}

/// Retry from a long-lived host (the daemon): the new task runs on a host
/// thread (`AgentManager::start`), as `agents_run` does; the CLI detaches a
/// worker process for the same prepared task instead.
pub fn retry_start(kind: AdapterKind, task_id: &str, workspace: &Path) -> R<Value> {
    let manager = manager(kind, workspace)?;
    let old = retry_source(&manager, task_id)?;
    let run = manager.start(&old.agent, &old.prompt).map_err(msg)?;
    encode(&run)
}

/// `susi mcp-add <name> <command-or-url> [args..]`: admit an MCP server
/// into the registry and hot-plug it into the live capability registry.
/// Returns the admission message; a refusal is an error.
pub fn mcp_add(name: &str, command_or_url: &str, args: &[String]) -> R<String> {
    let res = susi_tools::GmcpClient::admit_mcp_server(name, command_or_url, args);
    if !res.starts_with("SUCCESS") {
        return Err(res);
    }
    susi_gmcp::mcp_wrapper::auto_discover_mcp();
    susi_gemi::engines::mcp_provider::register_mcp_inference_providers(
        susi_gemi::susi_core::registry::CapabilityRegistry::global(),
    );
    Ok(res)
}

// ── socket dispatch ────────────────────────────────────────────────────────

fn field<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Dispatch one `daemon.adapters` request: `{action, kind, id, ..}`, with
/// `workspace` the (absolute) workspace the tool resolved.
pub fn dispatch(payload: &Value) -> R<Value> {
    let workspace = field(payload, "workspace")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(susi_paths::SusiDirs::substrate_home);
    let kind = || AdapterKind::parse(field(payload, "kind").ok_or("kind is required")?);
    let id = || field(payload, "id").ok_or_else(|| "id is required".to_string());
    match field(payload, "action").ok_or("action is required")? {
        "doctor" => {
            let kind = kind()?;
            let rows = doctor(kind, field(payload, "id"), &workspace)?;
            Ok(json!({ "all_ready": all_ready(&rows), "rows": rows }))
        }
        "setup" => setup(kind()?, id()?, &workspace),
        "configure" => {
            let kind = kind()?;
            let id = id()?;
            let document = match (payload.get("adapter"), field(payload, "file")) {
                (Some(doc), None) if doc.is_object() => doc.clone(),
                (None, Some(file)) => {
                    let file = Path::new(file);
                    if !file.is_absolute() {
                        return Err("file must be an absolute path (the socket has no caller working directory)".to_string());
                    }
                    let bytes =
                        std::fs::read(file).map_err(|e| format!("read {}: {e}", file.display()))?;
                    serde_json::from_slice(&bytes)
                        .map_err(|e| format!("{} is not JSON: {e}", file.display()))?
                }
                _ => return Err("pass exactly one of adapter (object) or file".to_string()),
            };
            // The socket tool's MAC check sees only an inline adapter; a
            // stored adapter is run later, so the document itself (inline
            // or read from `file`) must pass the same check: a program
            // needs process.exec, a remote A2A agent network.egress.
            susi_core::mac_policy::MacPolicy::global()
                .authorize_tool(
                    "adapter_configure",
                    &json!({ "kind": field(payload, "kind"), "adapter": document }),
                    &workspace,
                    None,
                )
                .map_err(msg)?;
            configure(kind, id, document, &workspace)
        }
        "reset" => reset(kind()?, id()?, &workspace),
        "retry" => {
            let kind = kind()?;
            if !matches!(kind, AdapterKind::Agents | AdapterKind::Frameworks) {
                return Err("retry applies to agents and frameworks tasks".to_string());
            }
            retry_start(
                kind,
                field(payload, "task_id").ok_or("task_id is required")?,
                &workspace,
            )
        }
        "mcp_add" => {
            let args: Vec<String> = match payload.get("args") {
                None | Some(Value::Null) => Vec::new(),
                Some(Value::Array(items)) => items
                    .iter()
                    .map(|v| v.as_str().map(str::to_string).ok_or("args must be strings"))
                    .collect::<Result<_, _>>()?,
                Some(_) => return Err("args must be an array of strings".to_string()),
            };
            let message = mcp_add(
                field(payload, "name").ok_or("name is required")?,
                field(payload, "command_or_url").ok_or("command_or_url is required")?,
                &args,
            )?;
            Ok(json!({ "message": message }))
        }
        other => Err(format!("unknown adapters action `{other}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_parse_and_name_their_rows() {
        assert_eq!(AdapterKind::parse("Agent").unwrap(), AdapterKind::Agents);
        assert_eq!(
            AdapterKind::parse("frameworks").unwrap(),
            AdapterKind::Frameworks
        );
        assert_eq!(AdapterKind::parse("models").unwrap(), AdapterKind::Models);
        assert_eq!(AdapterKind::parse("mcp").unwrap(), AdapterKind::Mcp);
        assert!(AdapterKind::parse("plugins").is_err());
        assert_eq!(AdapterKind::Frameworks.id_key(), "engine");
        assert_eq!(
            needs_setup_message(AdapterKind::Mcp),
            "one or more leading MCP servers need setup"
        );
    }

    #[test]
    fn dispatch_validates_arguments_before_touching_state() {
        let refused = |payload: Value| dispatch(&payload).unwrap_err();
        assert!(refused(json!({})).contains("action"));
        assert!(refused(json!({"action": "doctor"})).contains("kind"));
        assert!(refused(json!({"action": "setup", "kind": "agents"})).contains("id"));
        assert!(
            refused(json!({"action": "reset", "kind": "nope", "id": "x"}))
                .contains("unknown adapter")
        );
        assert!(
            refused(json!({"action": "configure", "kind": "mcp", "id": "x", "file": "rel.json"}))
                .contains("absolute")
        );
        assert!(
            refused(json!({"action": "configure", "kind": "mcp", "id": "x"}))
                .contains("exactly one")
        );
        assert!(
            refused(json!({"action": "retry", "kind": "models", "task_id": "t"}))
                .contains("retry applies")
        );
        assert!(refused(json!({"action": "retry", "kind": "agents"})).contains("task_id"));
        assert!(refused(json!({"action": "mcp_add", "name": "x"})).contains("command_or_url"));
        assert!(
            refused(json!({"action": "mcp_add", "name": "x", "command_or_url": "y", "args": [1]}))
                .contains("strings")
        );
        assert!(refused(json!({"action": "explode"})).contains("unknown adapters action"));
    }

    #[test]
    fn all_ready_needs_every_row_ready() {
        assert!(all_ready(&[json!({"prerequisites_present": true})]));
        assert!(!all_ready(&[
            json!({"prerequisites_present": true}),
            json!({"prerequisites_present": false})
        ]));
    }

    #[test]
    fn retry_refuses_a_task_that_does_not_exist() {
        let ws = std::env::temp_dir().join(format!("susi-adapter-retry-{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        assert!(retry_start(AdapterKind::Agents, "no-such-task", &ws).is_err());
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn id_key_and_needs_setup_message_cover_all_adapter_families() {
        assert_eq!(AdapterKind::Agents.id_key(), "agent");
        assert_eq!(AdapterKind::Frameworks.id_key(), "engine");
        assert_eq!(AdapterKind::Models.id_key(), "model");
        assert_eq!(AdapterKind::Mcp.id_key(), "server");
        assert!(needs_setup_message(AdapterKind::Agents).contains("need setup"));
        assert!(needs_setup_message(AdapterKind::Frameworks).contains("need setup"));
        assert!(needs_setup_message(AdapterKind::Models).contains("need setup"));
        assert!(needs_setup_message(AdapterKind::Mcp).contains("need setup"));
    }
}
