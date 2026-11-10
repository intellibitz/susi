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
mod tests {
    use super::*;

    #[test]
    fn mcp_launcher_native_lookup_does_not_execute_or_accept_relative_path() {
        let dir = susi_paths::tempfile::tempdir().unwrap();
        // Node ships an extensionless Unix shell launcher next to npx.cmd.
        // Native Windows must choose the .cmd, not try executing that script.
        fs::write(dir.path().join("npx"), "#!/bin/sh\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir.path().join("npx"), fs::Permissions::from_mode(0o700)).unwrap();
        }
        for name in ["npx.cmd", "uvx.exe"] {
            let path = dir.path().join(name);
            // Deliberately invalid executables: discovery must not run them.
            fs::write(&path, "not an executable program").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            }
            let stem = name.split('.').next().unwrap();
            assert_eq!(
                resolve_runner_in(stem, std::iter::once(dir.path().to_path_buf()), true),
                Some(path)
            );
        }
        assert!(resolve_runner_in("npx", std::iter::once(PathBuf::from(".")), true).is_none());
        assert!(
            resolve_runner_in("missing", std::iter::once(dir.path().to_path_buf()), true).is_none()
        );
    }

    #[test]
    fn damaged_user_disabled_list_is_an_error_not_empty() {
        let dir = std::env::temp_dir().join(format!(
            "susi_user_disabled_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        assert!(load_user_disabled(&dir).unwrap().is_empty());
        mark_user_disabled(&dir, "github").unwrap();
        mark_user_disabled(&dir, "sentry").unwrap();
        assert_eq!(load_user_disabled(&dir).unwrap(), ["github", "sentry"]);
        fs::write(user_disabled_path(&dir), "[\"github\"").unwrap();
        assert!(load_user_disabled(&dir).is_err());
        assert!(mark_user_disabled(&dir, "context7").is_err());
        assert!(clear_user_disabled(&dir, "github").is_err());
        assert_eq!(
            fs::read_to_string(user_disabled_path(&dir)).unwrap(),
            "[\"github\""
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// A synthetic pack-format catalog row (the shape commercial packs ship
    /// in `leading-mcp.json`); core bundles no real one.
    fn synthetic_definition() -> LeadingMcpDefinition {
        serde_json::from_value(serde_json::json!({
            "id": "acme-files",
            "name": "Acme Files MCP",
            "rank": 1,
            "documentation": "https://example.invalid/acme-files",
            "package": "@example/acme-files-mcp",
            "runner": "npx",
            "args": ["-y", "@example/acme-files-mcp", "{workspace}"],
            "env_keys": ["ACME_TOKEN"],
            "category": "fs"
        }))
        .expect("synthetic leading-mcp row")
    }

    #[test]
    fn pack_catalog_rows_parse_and_validate() {
        let def = synthetic_definition();
        def.validate().unwrap();
        assert_eq!(def.id, "acme-files");
        let mut bad = def.clone();
        bad.runner = String::new();
        assert!(bad.validate().is_err());
        let mut bad = def;
        bad.env_keys = vec!["NOT-A-NAME".into()];
        assert!(bad.validate().is_err());
    }

    #[test]
    fn expand_workspace_and_env_placeholders() {
        // Every env writer in this crate's tests holds ENV_LOCK: a concurrent
        // setenv can make another thread's getenv (HOME / XDG_CONFIG_HOME
        // path resolution) miss, which sent client::tests' second MCP admit
        // to a different config file.
        let _env = crate::susi_core::commit_log::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("SUSI_TEST_PG", "postgresql://localhost/db");
        let args = expand_args(
            &[
                "{workspace}".into(),
                "{env:SUSI_TEST_PG}".into(),
                "literal".into(),
            ],
            Path::new("/tmp"),
        )
        .unwrap();
        assert!(args[0].contains("tmp") || args[0] == "/tmp");
        assert_eq!(args[1], "postgresql://localhost/db");
        assert_eq!(args[2], "literal");
        std::env::remove_var("SUSI_TEST_PG");
    }

    #[test]
    fn expand_workspace_embedded_in_path_args() {
        let args = expand_args(
            &["--db-path".into(), "{workspace}/.susi/agent.sqlite".into()],
            Path::new("/tmp/ws"),
        )
        .unwrap();
        assert_eq!(args[1], "/tmp/ws/.susi/agent.sqlite");
        // {env:} stays exact-match only — secrets must not splice into
        // compound strings.
        let _env = crate::susi_core::commit_log::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("SUSI_TEST_EMBED", "sekrit");
        let args = expand_args(&["x-{env:SUSI_TEST_EMBED}".into()], Path::new("/tmp")).unwrap();
        assert_eq!(args[0], "x-{env:SUSI_TEST_EMBED}");
        std::env::remove_var("SUSI_TEST_EMBED");
    }

    #[test]
    fn override_store_round_trip() {
        let root = std::env::temp_dir().join(format!(
            "susi-leading-mcp-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        // Exercise the per-id override store on a synthetic pack row without
        // touching the user's real catalog or mcp_config.
        let overrides = root.join("overrides");
        let def = synthetic_definition();
        let over = LeadingMcpOverride {
            runner: Some("uvx".into()),
            args: Some(vec!["acme-files-mcp".into()]),
            env_keys: Some(vec![]),
            package: None,
            ..Default::default()
        };
        crate::susi_config::write_json_override(&overrides, &def.id, &over).unwrap();
        let stored: LeadingMcpOverride =
            serde_json::from_slice(&fs::read(overrides.join("acme-files.json")).unwrap()).unwrap();
        assert_eq!(stored.runner.as_deref(), Some("uvx"));
        crate::susi_config::clear_json_override(&overrides, &def.id).unwrap();
        assert!(!overrides.join("acme-files.json").exists());
        let _ = fs::remove_dir_all(root);
    }

    /// The explicit admission path from an installed pack: with a synthetic
    /// pack's one-row `leading-mcp.json` loaded, nothing is enabled on its
    /// own; `configure` / `reset` touch only the per-id override; `enable`
    /// writes exactly that server and `disable` removes it. A configured
    /// server with no catalog entry is listed and removable too.
    #[test]
    fn manager_admits_a_pack_row_only_on_explicit_request() {
        let Some(dir) = crate::private_config_fixture() else {
            return;
        };
        let bin = dir.join("acme-runner");
        fs::write(&bin, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let pack = dir.join("extensions").join("acme");
        fs::create_dir_all(&pack).unwrap();
        fs::write(
            pack.join("manifest.json"),
            serde_json::json!({
                "id": "acme", "name": "Acme", "version": "0.0.1",
                "files": { "leading-mcp.json": "leading-mcp.json" }
            })
            .to_string(),
        )
        .unwrap();
        let mut row = serde_json::to_value(synthetic_definition()).unwrap();
        row["runner"] = serde_json::json!(bin.display().to_string());
        row["env_keys"] = serde_json::json!([]);
        fs::write(
            pack.join("leading-mcp.json"),
            serde_json::json!([row]).to_string(),
        )
        .unwrap();
        crate::susi_sandbox::extensions::load_pack("acme").unwrap();

        let ws = dir.join("ws");
        fs::create_dir_all(&ws).unwrap();
        let ws = ws.canonicalize().unwrap();
        let overrides = dir.join("leading-mcp");
        let manager = LeadingMcpManager::with_config(ws.clone(), overrides.clone());
        let mcp_config = GmcpClient::get_config_path();
        assert!(!manager.is_enabled("acme-files").unwrap());
        assert!(!mcp_config.exists(), "nothing is admitted on its own");

        let over = LeadingMcpOverride {
            args: Some(vec!["--stdio".into()]),
            ..Default::default()
        };
        assert_eq!(
            manager.configure("acme-files", &over).unwrap().args,
            ["--stdio"]
        );
        assert!(overrides.join("acme-files.json").is_file());
        assert_eq!(manager.effective("acme-files").unwrap().args, ["--stdio"]);
        assert!(!mcp_config.exists(), "configure enables nothing");
        let reset = manager.reset("acme-files").unwrap();
        assert_eq!(reset.args, ["-y", "@example/acme-files-mcp", "{workspace}"]);
        assert!(!overrides.join("acme-files.json").exists());

        let srv = manager.enable("acme-files").unwrap();
        assert_eq!(srv.command, bin.display().to_string());
        assert_eq!(srv.args.last().unwrap(), &ws.display().to_string());
        let cfg = GmcpClient::read_mcp_config(&mcp_config).unwrap();
        assert_eq!(cfg.mcp_servers.len(), 1);
        assert!(cfg.mcp_servers.contains_key("acme-files"));

        assert!(manager.disable("acme-files").unwrap());
        assert!(GmcpClient::read_mcp_config(&mcp_config)
            .unwrap()
            .mcp_servers
            .is_empty());
        assert!(load_user_disabled(&overrides)
            .unwrap()
            .contains(&"acme-files".to_string()));

        let admitted = GmcpClient::admit_mcp_server("legacy", &bin.display().to_string(), &[]);
        assert_eq!(admitted, "SUCCESS_ADMITTED");
        let rows = manager.status().unwrap();
        assert!(
            rows.iter().any(|r| r["mcp"]["id"] == "legacy"
                && r["catalog"] == false
                && r["enabled"] == true),
            "{rows:?}"
        );
        assert!(manager.disable("legacy").unwrap());
        assert!(manager.disable("not-configured").is_err());
    }

    #[test]
    fn enabled_config_launcher_overrides_catalog_runner() {
        let root = std::env::temp_dir().join(format!(
            "susi-mcp-preflight-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let bin = root.join("npx");
        fs::write(&bin, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&bin).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&bin, perms).unwrap();
        }
        let def = LeadingMcpDefinition {
            id: "github".into(),
            name: "GitHub MCP".into(),
            rank: 2,
            documentation: String::new(),
            package: "ghcr.io/github/github-mcp-server".into(),
            runner: "docker".into(),
            args: vec!["run".into()],
            env_keys: vec!["SUSI_TEST_MCP_ONLY".into()],
            category: "vcs".into(),
            notes: String::new(),
        };
        let cfg = McpServerConfig {
            command: bin.display().to_string(),
            args: vec!["-y".into(), "@modelcontextprotocol/server-github".into()],
            env: Some(HashMap::from([(
                "SUSI_TEST_MCP_ONLY".into(),
                "present".into(),
            )])),
            extra: HashMap::new(),
        };
        let msg = preflight_definition(&def, Some(&cfg), &root).unwrap();
        assert!(msg.contains("enabled config"), "{msg}");
        let err = preflight_definition(&def, None, &root)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("docker") || err.contains("SUSI_TEST_MCP_ONLY"),
            "{err}"
        );
        let _ = fs::remove_dir_all(root);
    }
}
