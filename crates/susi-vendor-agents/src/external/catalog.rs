use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use susi_error::{eai_bail as bail, EaiResult as Result, ResultExt as Context};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogKind {
    /// Top coding executors (Claude Code, Cursor, …).
    Execution,
    /// Top agent frameworks / engines (LangGraph, CrewAI, …).
    Framework,
}

impl CatalogKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Execution => "agent",
            Self::Framework => "framework",
        }
    }

    pub fn list_hint(self) -> &'static str {
        match self {
            Self::Execution => "`susi agents list`",
            Self::Framework => "`susi frameworks list`",
        }
    }

    pub fn config_subdir(self) -> &'static str {
        match self {
            Self::Execution => "execution-agents",
            Self::Framework => "agent-engines",
        }
    }

    pub fn run_subdir(self) -> &'static str {
        match self {
            Self::Execution => "execution-agents",
            Self::Framework => "agent-engines",
        }
    }

    fn bundled(self) -> &'static str {
        match self {
            Self::Execution => include_str!("../../../../config/execution-agents.json"),
            Self::Framework => include_str!("../../../../config/agent-engines.json"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Adapter {
    Command {
        program: String,
        args: Vec<String>,
    },
    Qwen {
        python: String,
    },
    /// User-authored framework entry (script or module:callable) via absolute config JSON.
    Python {
        python: String,
        /// Import name checked at doctor/preflight time (e.g. `langgraph`, `pydantic_ai`).
        import_name: String,
        config_env: String,
    },
    Devin {
        api_key_env: String,
        /// Env var holding the Devin organization id (`org-…`). Defaults to `DEVIN_ORG_ID`.
        #[serde(default = "default_devin_org_id_env")]
        org_id_env: String,
    },
    Manus {
        api_key_env: String,
    },
    /// Remote A2A-compliant agent, discovered from `<url>/.well-known/agent-card.json`.
    A2a {
        url: String,
        /// Env var holding a bearer token, when the remote agent requires one.
        #[serde(default)]
        token_env: Option<String>,
    },
    /// OpenHands app-server or Cloud REST API (`/api/v1/app-conversations`).
    OpenHandsServer {
        base_url: String,
        /// Env var holding the OpenHands API key (bearer), if the server needs one.
        #[serde(default)]
        api_key_env: Option<String>,
    },
    /// Agent Client Protocol agent spoken to over stdio (JSON-RPC).
    Acp {
        program: String,
        args: Vec<String>,
    },
}

fn default_devin_org_id_env() -> String {
    "DEVIN_ORG_ID".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDefinition {
    pub id: String,
    pub name: String,
    pub peer_name: String,
    /// User-supplied catalog order, not a measured quality score.
    pub rank: u8,
    pub documentation: String,
    pub adapter: Adapter,
    /// Mandate 35: preserve unknown catalog keys from extension packs.
    #[serde(flatten, default)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

pub fn catalog(kind: CatalogKind) -> Result<Vec<AgentDefinition>> {
    let name = match kind {
        CatalogKind::Execution => "execution-agents.json",
        CatalogKind::Framework => "agent-engines.json",
    };
    Ok(crate::susi_sandbox::extensions::load_json_or_bundled(
        name,
        kind.bundled(),
    ))
}

pub fn definition(kind: CatalogKind, id: &str) -> Result<AgentDefinition> {
    catalog(kind)?
        .into_iter()
        .find(|a| a.id == id || a.peer_name == id)
        .with_context(|| format!("unknown {} {id}; use {}", kind.label(), kind.list_hint()))
}

/// Resolve a managed peer name against executors first, then frameworks.
pub fn resolve_managed(id: &str) -> Result<(CatalogKind, AgentDefinition)> {
    if let Ok(def) = definition(CatalogKind::Execution, id) {
        return Ok((CatalogKind::Execution, def));
    }
    definition(CatalogKind::Framework, id).map(|def| (CatalogKind::Framework, def))
}

impl Adapter {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Command { program, args } => {
                if program.trim().is_empty()
                    || program.contains('\0')
                    || args.iter().any(|a| a.contains('\0'))
                {
                    bail!("command and arguments must be nonempty executable / NUL-free argv");
                }
            }
            Self::Qwen { python } if python.trim().is_empty() => {
                bail!("python executable is required")
            }
            Self::Python {
                python,
                import_name,
                config_env,
            } => {
                if python.trim().is_empty() {
                    bail!("python executable is required");
                }
                if import_name.is_empty()
                    || !import_name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.')
                {
                    bail!("import_name must be a dotted Python module name");
                }
                if config_env.is_empty()
                    || !config_env
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                {
                    bail!(
                        "config_env must name an environment variable, not contain a path or secret"
                    );
                }
            }
            Self::Devin {
                api_key_env,
                org_id_env,
            } => {
                if api_key_env.is_empty()
                    || !api_key_env
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                {
                    bail!(
                        "api_key_env must name an environment variable, not contain a credential"
                    );
                }
                if org_id_env.is_empty()
                    || !org_id_env
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                {
                    bail!("org_id_env must name an environment variable, not contain an org id");
                }
            }
            Self::Manus { api_key_env } => {
                if api_key_env.is_empty()
                    || !api_key_env
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                {
                    bail!(
                        "api_key_env must name an environment variable, not contain a credential"
                    );
                }
            }
            Self::A2a { url, token_env } => {
                if !(url.starts_with("https://") || url.starts_with("http://"))
                    || url.contains('\0')
                {
                    bail!("A2A url must be an http(s) URL");
                }
                if token_env.as_deref().is_some_and(|e| {
                    e.is_empty() || !e.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                }) {
                    bail!("token_env must name an environment variable, not contain a credential");
                }
            }
            Self::OpenHandsServer {
                base_url,
                api_key_env,
            } => {
                if !(base_url.starts_with("https://") || base_url.starts_with("http://"))
                    || base_url.contains('\0')
                {
                    bail!("OpenHands base_url must be an http(s) URL");
                }
                if api_key_env.as_deref().is_some_and(|e| {
                    e.is_empty() || !e.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                }) {
                    bail!(
                        "api_key_env must name an environment variable, not contain a credential"
                    );
                }
            }
            Self::Acp { program, args } => {
                if program.trim().is_empty()
                    || program.contains('\0')
                    || args.iter().any(|a| a.contains('\0'))
                {
                    bail!("ACP command and arguments must be nonempty executable / NUL-free argv");
                }
            }
            Self::Qwen { .. } => {}
        }
        Ok(())
    }

    /// Local preflight only. Presence of a binary/package does not prove vendor authentication.
    pub fn preflight(&self) -> Result<String> {
        self.validate()?;
        match self {
            Self::Command { program, .. } => {
                resolve_program(program)
                    .with_context(|| format!("missing executable: {program}"))?;
                Ok("executable found; vendor login/permissions must be configured".into())
            }
            Self::A2a { url, token_env } => {
                if let Some(env) = token_env {
                    if crate::susi_config::env_or_cloud_env(env)
                        .unwrap_or_default()
                        .trim()
                        .is_empty()
                    {
                        bail!("set {env} for the remote A2A agent's bearer token");
                    }
                }
                Ok(format!(
                    "A2A endpoint {url}; agent card is fetched at execution"
                ))
            }
            Self::OpenHandsServer {
                base_url,
                api_key_env,
            } => {
                if let Some(env) = api_key_env {
                    if crate::susi_config::env_or_cloud_env(env)
                        .unwrap_or_default()
                        .trim()
                        .is_empty()
                    {
                        bail!("set {env} for the OpenHands API");
                    }
                }
                Ok(format!(
                    "OpenHands endpoint {base_url}; API access checked at execution"
                ))
            }
            Self::Acp { program, .. } => {
                resolve_program(program)
                    .with_context(|| format!("missing ACP executable: {program}"))?;
                Ok(
                    "ACP executable found; agent auth is checked by the initialize handshake"
                        .into(),
                )
            }
            Self::Qwen { python } => {
                resolve_program(python).context("Python executable missing")?;
                if std::env::var("SUSI_QWEN_CONFIG")
                    .unwrap_or_default()
                    .is_empty()
                {
                    bail!("set SUSI_QWEN_CONFIG to an absolute JSON configuration file for Qwen-Agent");
                }
                Ok("Python/config present; qwen_agent package and model access checked at execution".into())
            }
            Self::Python {
                python,
                import_name,
                config_env,
            } => {
                let program = resolve_program(python).context("Python executable missing")?;
                let status = crate::susi_core::bounded_cmd::output_within(
                    Command::new(&program).args(["-c", &format!("import {import_name}")]),
                    std::time::Duration::from_secs(10),
                )
                .with_context(|| format!("failed to probe import {import_name}"))?
                .status;
                if !status.success() {
                    bail!("Python package not importable: {import_name}");
                }
                let path = std::env::var(config_env).unwrap_or_default();
                if path.trim().is_empty() {
                    bail!("set {config_env} to an absolute JSON config with script or entrypoint");
                }
                let config = Path::new(&path);
                if !config.is_absolute() {
                    bail!("{config_env} must be an absolute path");
                }
                if !config.is_file() {
                    bail!("{config_env} does not point to an existing file");
                }
                Ok("Python package and entry config present; model credentials checked at execution".into())
            }
            Self::Devin {
                api_key_env,
                org_id_env,
            } => {
                if crate::susi_config::env_or_cloud_env(api_key_env)
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
                {
                    bail!("set {api_key_env} for cloud API access");
                }
                if crate::susi_config::env_or_cloud_env(org_id_env)
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
                {
                    bail!("set {org_id_env} (Devin org id, e.g. org-…) for API v3");
                }
                Ok("credential and org id present; API access checked at execution".into())
            }
            Self::Manus { api_key_env } => {
                if crate::susi_config::env_or_cloud_env(api_key_env)
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
                {
                    bail!("set {api_key_env} for cloud API access");
                }
                Ok("credential present; API access checked at execution".into())
            }
        }
    }

    pub fn is_cloud(&self) -> bool {
        matches!(
            self,
            Self::Devin { .. }
                | Self::Manus { .. }
                | Self::A2a { .. }
                | Self::OpenHandsServer { .. }
        )
    }
}

pub fn resolve_program(program: &str) -> Option<PathBuf> {
    fn executable(p: &Path) -> bool {
        if !p.is_file() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            p.metadata()
                .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
        }
        #[cfg(not(unix))]
        true
    }
    let p = Path::new(program);
    if p.components().count() > 1 {
        return executable(p).then(|| p.to_path_buf());
    }
    for dir in std::env::split_paths(&std::env::var_os("PATH")?) {
        // Never silently discover a repo-supplied binary via a relative PATH entry.
        if !dir.is_absolute() {
            continue;
        }
        let p = dir.join(program);
        if executable(&p) {
            return Some(p);
        }
        #[cfg(windows)]
        for ext in ["exe", "cmd", "bat"] {
            let p = dir.join(format!("{program}.{ext}"));
            if executable(&p) {
                return Some(p);
            }
        }
    }
    None
}
