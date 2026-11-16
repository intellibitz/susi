//! Durable management of the top MCP tool servers agents call.
//!
//! Core bundles no server catalog (zero-UI plan A9): the catalog is the
//! active extension pack's `leading-mcp.json`, empty when none is installed.
//! Core never enables a server on its own (A8) — `enable` runs only on an
//! explicit `susi mcp enable` / `tools.leading.enable` request.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use susi_error::{eai_bail as bail, EaiResult as Result, ResultExt as Context};

use crate::config::{McpConfig, McpServerConfig};
use crate::GmcpClient;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeadingMcpDefinition {
    pub id: String,
    pub name: String,
    pub rank: u8,
    pub documentation: String,
    pub package: String,
    /// Preferred launcher: `npx` or `uvx` (host may override).
    pub runner: String,
    pub args: Vec<String>,
    #[serde(default)]
    pub env_keys: Vec<String>,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LeadingMcpOverride {
    pub runner: Option<String>,
    pub args: Option<Vec<String>>,
    pub env_keys: Option<Vec<String>>,
    pub package: Option<String>,
    /// Mandate 35: preserve unknown keys on configure read-modify-write.
    #[serde(flatten, default)]
    pub extra: HashMap<String, serde_json::Value>,
}

#[derive(Clone)]
pub struct LeadingMcpManager {
    config: PathBuf,
    workspace: PathBuf,
}

impl LeadingMcpManager {
    pub fn new(workspace: &Path) -> Result<Self> {
        let workspace = workspace
            .canonicalize()
            .context("MCP workspace does not exist")?;
        Ok(Self {
            config: susi_paths::SusiDirs::config_dir().join("leading-mcp"),
            workspace,
        })
    }

    pub fn with_config(workspace: PathBuf, config: PathBuf) -> Self {
        Self { config, workspace }
    }

    /// The installed catalog: the active extension pack's `leading-mcp.json`
    /// (empty with no pack installed).
    pub fn catalog() -> Result<Vec<LeadingMcpDefinition>> {
        Ok(crate::susi_sandbox::extensions::load_json_or_bundled(
            "leading-mcp.json",
            "[]",
        ))
    }

    pub fn definition(id: &str) -> Result<LeadingMcpDefinition> {
        Self::catalog()?
            .into_iter()
            .find(|m| m.id == id || m.name.eq_ignore_ascii_case(id))
            .with_context(|| {
                format!(
                "unknown leading MCP {id}; use `susi mcp list` (catalogs come from an installed \
                 extension pack), or admit any server explicitly with `susi mcp-add`"
            )
            })
    }

    pub fn effective(&self, id: &str) -> Result<LeadingMcpDefinition> {
        let mut def = Self::definition(id)?;
        let path = self.config.join(format!("{}.json", def.id));
        if let Ok(bytes) = fs::read(path) {
            let over: LeadingMcpOverride =
                serde_json::from_slice(&bytes).context("invalid leading-mcp override")?;
            if let Some(runner) = over.runner.filter(|s| !s.trim().is_empty()) {
                def.runner = runner;
            }
            if let Some(args) = over.args {
                def.args = args;
            }
            if let Some(env_keys) = over.env_keys {
                def.env_keys = env_keys;
            }
            if let Some(package) = over.package.filter(|s| !s.trim().is_empty()) {
                def.package = package;
            }
        }
        def.validate()?;
        Ok(def)
    }

    pub fn configure(&self, id: &str, over: &LeadingMcpOverride) -> Result<LeadingMcpDefinition> {
        let def = Self::definition(id)?;
        crate::susi_config::write_json_override(&self.config, &def.id, over)?;
        self.effective(&def.id)
    }

    pub fn reset(&self, id: &str) -> Result<LeadingMcpDefinition> {
        let def = Self::definition(id)?;
        crate::susi_config::clear_json_override(&self.config, &def.id)?;
        self.effective(&def.id)
    }

    /// Local readiness — does not start the MCP server or call remote APIs.
    /// An already-enabled server is judged by the command saved in
    /// `mcp_config.json` (for example `npx`), not the catalog runner
    /// (`docker`) that was never what the user enabled.
    pub fn preflight(&self, id: &str) -> Result<String> {
        let def = self.effective(id)?;
        let enabled = self.enabled_config(&def.id)?;
        preflight_definition(&def, enabled.as_ref(), &self.workspace)
    }

    pub fn is_enabled(&self, id: &str) -> Result<bool> {
        Ok(load_mcp_config()?.mcp_servers.contains_key(id))
    }

    pub fn enabled_config(&self, id: &str) -> Result<Option<McpServerConfig>> {
        Ok(load_mcp_config()?.mcp_servers.get(id).cloned())
    }

    pub fn enable(&self, id: &str) -> Result<McpServerConfig> {
        let def = self.effective(id)?;
        self.preflight(&def.id)?;
        let runner = resolve_runner(&def.runner).context("launcher missing after preflight")?;
        let args = expand_args(&def.args, &self.workspace)?;
        let mut env: HashMap<String, String> = HashMap::new();
        for key in &def.env_keys {
            if let Some(value) = env_value(key) {
                env.insert(key.clone(), value);
            }
        }
        let srv = McpServerConfig {
            command: runner.to_string_lossy().into_owned(),
            args,
            env: if env.is_empty() { None } else { Some(env) },
            extra: HashMap::new(),
        };
        let mut config = load_mcp_config()?;
        config.mcp_servers.insert(def.id.clone(), srv.clone());
        save_mcp_config(&config)?;
        clear_user_disabled(&self.config, &def.id)?;
        Ok(srv)
    }

    /// Remove a server from `mcp_config.json`. A configured server with no
    /// catalog entry (admitted with `susi mcp-add`, or enabled by an earlier
    /// core whose catalog is gone) is removed by its exact id too, so every
    /// configured server stays removable.
    pub fn disable(&self, id: &str) -> Result<bool> {
        let def_id = match Self::definition(id) {
            Ok(def) => def.id,
            Err(e) => {
                if !load_mcp_config()?.mcp_servers.contains_key(id) {
                    return Err(e);
                }
                id.to_string()
            }
        };
        let mut config = load_mcp_config()?;
        let removed = config.mcp_servers.remove(&def_id).is_some();
        if removed {
            save_mcp_config(&config)?;
        }
        // Persist the user's choice (`user_disabled.json`).
        mark_user_disabled(&self.config, &def_id)?;
        Ok(removed)
    }

    /// One row per catalog entry, then one per server configured in
    /// `mcp_config.json` without a catalog entry (`"catalog": false`), so
    /// every server core would launch on a tool listing is visible.
    pub fn status(&self) -> Result<Vec<serde_json::Value>> {
        let preferred = Self::catalog()?;
        let mut rows = Vec::new();
        let catalog_ids: Vec<String> = preferred.iter().map(|d| d.id.clone()).collect();
        for def in preferred {
            let effective = self.effective(&def.id).unwrap_or(def.clone());
            let readiness = self.preflight(&def.id);
            rows.push(serde_json::json!({
                "mcp": effective,
                "catalog": true,
                "enabled": self.is_enabled(&def.id).unwrap_or(false),
                "prerequisites_present": readiness.is_ok(),
                "detail": match readiness { Ok(s) => s, Err(e) => e.to_string() },
                "config": self.enabled_config(&def.id).ok().flatten(),
            }));
        }
        if let Ok(config) = load_mcp_config() {
            let mut extra: Vec<_> = config
                .mcp_servers
                .into_iter()
                .filter(|(id, _)| !catalog_ids.contains(id))
                .collect();
            extra.sort_by(|a, b| a.0.cmp(&b.0));
            for (id, cfg) in extra {
                rows.push(serde_json::json!({
                    "mcp": { "id": id },
                    "catalog": false,
                    "enabled": true,
                    "prerequisites_present": serde_json::Value::Null,
                    "detail": format!(
                        "configured in mcp_config.json without a catalog entry (admitted with `susi mcp-add`, or enabled by an earlier core); `susi mcp disable {id}` removes it"
                    ),
                    "config": cfg,
                }));
            }
        }
        Ok(rows)
    }
}

impl LeadingMcpDefinition {
    fn validate(&self) -> Result<()> {
        if self.id.trim().is_empty()
            || self.package.trim().is_empty()
            || self.runner.trim().is_empty()
        {
            bail!("leading MCP id, package, and runner must be nonempty");
        }
        if self.runner.contains('\0') || self.args.iter().any(|a| a.contains('\0')) {
            bail!("runner/args must be NUL-free");
        }
        for key in &self.env_keys {
            if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                bail!("env_keys must name environment variables");
            }
        }
        Ok(())
    }
}

/// The one strict reader (`GmcpClient::read_mcp_config`): a damaged or
/// unreadable config is an error, never an empty config that `enable` /
/// `disable` would then save over every other server.
fn load_mcp_config() -> Result<McpConfig> {
    let path = GmcpClient::get_config_path();
    GmcpClient::read_mcp_config(&path)
        .map_err(|code| susi_error::EaiError::internal(format!("{code}: {}", path.display())))
}

fn save_mcp_config(config: &McpConfig) -> Result<()> {
    let path = GmcpClient::get_config_path();
    if let Some(parent) = path.parent() {
        crate::susi_config::create_private_dir(parent)?;
    }
    crate::susi_config::atomic_write_json_pretty(&path, config)
}

fn env_satisfied(key: &str) -> bool {
    env_value(key).is_some()
}

fn env_value(key: &str) -> Option<String> {
    let mut candidates = vec![key.to_owned()];
    match key {
        "GITHUB_PERSONAL_ACCESS_TOKEN" => candidates.push("GITHUB_TOKEN".into()),
        "SENTRY_ACCESS_TOKEN" => {
            candidates.push("SENTRY_AUTH_TOKEN".into());
            candidates.push("SENTRY_TOKEN".into());
        }
        "POSTGRES_URL" => {
            candidates.push("DATABASE_URL".into());
            candidates.push("POSTGRES_CONNECTION_STRING".into());
        }
        _ => {}
    }
    for name in candidates {
        if let Ok(value) = crate::susi_config::env_or_cloud_env(&name) {
            if !value.trim().is_empty() {
                return Some(value);
            }
        }
    }
    None
}

fn expand_args(args: &[String], workspace: &Path) -> Result<Vec<String>> {
    let mut out = Vec::with_capacity(args.len());
    for arg in args {
        if arg.contains("{workspace}") {
            // Path-style args embed the placeholder ("{workspace}/.susi/x").
            // Substituting only exact matches let a literal "{workspace}/"
            // directory get created by the spawned server.
            out.push(arg.replace("{workspace}", &workspace.to_string_lossy()));
            continue;
        }
        if let Some(key) = arg.strip_prefix("{env:").and_then(|s| s.strip_suffix('}')) {
            if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                bail!("invalid env placeholder in MCP args");
            }
            let value = env_value(key)
                .with_context(|| format!("set {key} (required by MCP argv template)"))?;
            out.push(value);
            continue;
        }
        out.push(arg.clone());
    }
    Ok(out)
}

fn preflight_definition(
    def: &LeadingMcpDefinition,
    enabled: Option<&McpServerConfig>,
    workspace: &Path,
) -> Result<String> {
    if let Some(cfg) = enabled.filter(|cfg| !cfg.command.trim().is_empty()) {
        resolve_runner(&cfg.command)
            .with_context(|| format!("missing launcher executable: {}", cfg.command))?;
        for key in &def.env_keys {
            let in_cfg = cfg
                .env
                .as_ref()
                .is_some_and(|env| env.get(key).is_some_and(|value| !value.trim().is_empty()));
            if !in_cfg && !env_satisfied(key) {
                bail!("set {key} for {}", def.id);
            }
        }
        return Ok(format!(
            "launcher {} ready (enabled config); credentials present; enabled=true",
            cfg.command
        ));
    }
    resolve_runner(&def.runner)
        .with_context(|| format!("missing launcher executable: {}", def.runner))?;
    for key in &def.env_keys {
        if !env_satisfied(key) {
            bail!("set {key} for {}", def.id);
        }
    }
    let _ = expand_args(&def.args, workspace)?;
    let enabled_flag = enabled.is_some();
    Ok(format!(
        "launcher {} ready; credentials present; enabled={enabled_flag}",
        def.runner
    ))
}

/// Filesystem-only launcher discovery. Version processes do not establish MCP
/// readiness and must never delay admission; the real handshake verifies use.
pub(crate) fn resolve_runner(runner: &str) -> Option<PathBuf> {
    resolve_runner_in(
        runner,
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        cfg!(windows),
    )
}

fn resolve_runner_in(
    runner: &str,
    directories: impl Iterator<Item = PathBuf>,
    windows: bool,
) -> Option<PathBuf> {
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
    let p = Path::new(runner);
    if p.components().count() > 1 {
        return executable(p).then(|| p.to_path_buf());
    }
    // Prefer absolute PATH entries only (never repo-relative discovery).
    for dir in directories {
        if !dir.is_absolute() {
            continue;
        }
        let candidate = dir.join(runner);
        // npx is commonly a .cmd launcher; uvx/node are native .exe files.
        // Return the resolved path so launch uses precisely what was found.
        if windows && candidate.extension().is_none() {
            for suffix in ["exe", "com", "cmd", "bat"] {
                let candidate = candidate.with_extension(suffix);
                if executable(&candidate) {
                    return Some(candidate);
                }
            }
        } else if executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn user_disabled_path(config_dir: &Path) -> PathBuf {
    config_dir.join("user_disabled.json")
}

/// Servers the user disabled. Absent is an empty list; a damaged or
/// unreadable list is an error. Reading it as empty re-admitted every
/// server the user had turned off on the next auto-enable, and the next
/// disable rewrote the list with a single entry.
fn load_user_disabled(config_dir: &Path) -> Result<Vec<String>> {
    let path = user_disabled_path(config_dir);
    match fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| {
            susi_error::EaiError::internal(format!("damaged {}: {e}", path.display()))
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(susi_error::EaiError::internal(format!(
            "unreadable {}: {e}",
            path.display()
        ))),
    }
}

fn mark_user_disabled(config_dir: &Path, id: &str) -> Result<()> {
    crate::susi_config::create_private_dir(config_dir)?;
    let mut list = load_user_disabled(config_dir)?;
    if !list.iter().any(|x| x == id) {
        list.push(id.to_string());
    }
    crate::susi_config::atomic_write_json_pretty(&user_disabled_path(config_dir), &list)
}

fn clear_user_disabled(config_dir: &Path, id: &str) -> Result<()> {
    let mut list = load_user_disabled(config_dir)?;
    let before = list.len();
    list.retain(|x| x != id);
    if list.len() == before {
        return Ok(());
    }
    crate::susi_config::create_private_dir(config_dir)?;
    crate::susi_config::atomic_write_json_pretty(&user_disabled_path(config_dir), &list)
}

#[cfg(test)]
#[path = "tests/leading_mcp.rs"]
mod tests;
