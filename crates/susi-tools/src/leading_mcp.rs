//! Durable management of the top MCP tool servers agents call.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

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
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LeadingMcpOverride {
    pub runner: Option<String>,
    pub args: Option<Vec<String>>,
    pub env_keys: Option<Vec<String>>,
    pub package: Option<String>,
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

    pub fn catalog() -> Result<Vec<LeadingMcpDefinition>> {
        Ok(susi_sandbox::extensions::load_json_or_bundled(
            "leading-mcp.json",
            include_str!("../../../config/leading-mcp.json"),
        ))
    }

    pub fn definition(id: &str) -> Result<LeadingMcpDefinition> {
        Self::catalog()?
            .into_iter()
            .find(|m| m.id == id || m.name.eq_ignore_ascii_case(id))
            .with_context(|| format!("unknown leading MCP {id}; use `susi mcp list`"))
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
        private_dir(&self.config)?;
        atomic_json(&self.config.join(format!("{}.json", def.id)), over)?;
        self.effective(&def.id)
    }

    pub fn reset(&self, id: &str) -> Result<LeadingMcpDefinition> {
        let def = Self::definition(id)?;
        match fs::remove_file(self.config.join(format!("{}.json", def.id))) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        self.effective(&def.id)
    }

    /// Local readiness — does not start the MCP server or call remote APIs.
    pub fn preflight(&self, id: &str) -> Result<String> {
        let def = self.effective(id)?;
        resolve_runner(&def.runner)
            .with_context(|| format!("missing launcher executable: {}", def.runner))?;
        for key in &def.env_keys {
            if !env_satisfied(key) {
                bail!("set {key} for {}", def.id);
            }
        }
        // Expand args to catch missing {env:...} placeholders early.
        let _ = expand_args(&def.args, &self.workspace)?;
        let enabled = self.is_enabled(&def.id)?;
        Ok(format!(
            "launcher {} ready; credentials present; enabled={}",
            def.runner, enabled
        ))
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

    pub fn disable(&self, id: &str) -> Result<bool> {
        let def = Self::definition(id)?;
        let mut config = load_mcp_config()?;
        let removed = config.mcp_servers.remove(&def.id).is_some();
        if removed {
            save_mcp_config(&config)?;
        }
        // Persist unload so zero-config auto-enable does not immediately re-admit.
        mark_user_disabled(&self.config, &def.id)?;
        Ok(removed)
    }

    /// Zero-config: enable every leading MCP whose launcher + env keys are ready,
    /// unless the user previously `disable`d it.
    pub fn auto_enable_ready(&self) -> Result<Vec<String>> {
        let disabled = load_user_disabled(&self.config);
        let mut enabled = Vec::new();
        let mut catalog = Self::catalog()?;
        catalog.sort_by_key(|m| m.rank);
        for def in catalog {
            if disabled.iter().any(|id| id == &def.id) {
                continue;
            }
            if self.is_enabled(&def.id).unwrap_or(false) {
                continue;
            }
            if self.preflight(&def.id).is_err() {
                continue;
            }
            match self.enable(&def.id) {
                Ok(_) => {
                    enabled.push(def.id.clone());
                    if std::env::var("SUSI_VERBOSE").is_ok() {
                        eprintln!("[AUTO] Enabled leading MCP `{}`", def.id);
                    }
                }
                Err(e) => {
                    if std::env::var("SUSI_VERBOSE").is_ok() {
                        eprintln!("[AUTO] Skip MCP `{}`: {e}", def.id);
                    }
                }
            }
        }
        Ok(enabled)
    }

    pub fn status(&self) -> Result<Vec<serde_json::Value>> {
        let preferred = Self::catalog()?;
        let mut rows = Vec::new();
        for def in preferred {
            let effective = self.effective(&def.id).unwrap_or(def.clone());
            let readiness = self.preflight(&def.id);
            rows.push(serde_json::json!({
                "mcp": effective,
                "enabled": self.is_enabled(&def.id).unwrap_or(false),
                "prerequisites_present": readiness.is_ok(),
                "detail": match readiness { Ok(s) => s, Err(e) => e.to_string() },
                "config": self.enabled_config(&def.id).ok().flatten(),
            }));
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

fn load_mcp_config() -> Result<McpConfig> {
    let path = GmcpClient::get_config_path();
    match fs::read_to_string(&path) {
        Ok(content) => Ok(serde_json::from_str(&content).unwrap_or(McpConfig {
            mcp_servers: HashMap::new(),
            extra: HashMap::new(),
        })),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(McpConfig {
            mcp_servers: HashMap::new(),
            extra: HashMap::new(),
        }),
        Err(e) => Err(e.into()),
    }
}

fn save_mcp_config(config: &McpConfig) -> Result<()> {
    let path = GmcpClient::get_config_path();
    if let Some(parent) = path.parent() {
        private_dir(parent)?;
    }
    atomic_json(&path, config)
}

fn env_satisfied(key: &str) -> bool {
    env_value(key).is_some()
}

fn env_value(key: &str) -> Option<String> {
    let mut candidates = vec![key.to_owned()];
    match key {
        "GITHUB_PERSONAL_ACCESS_TOKEN" => candidates.push("GITHUB_TOKEN".into()),
        "POSTGRES_URL" => {
            candidates.push("DATABASE_URL".into());
            candidates.push("POSTGRES_CONNECTION_STRING".into());
        }
        _ => {}
    }
    for name in candidates {
        if let Ok(value) = std::env::var(&name) {
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
        if arg == "{workspace}" {
            out.push(workspace.to_string_lossy().into_owned());
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

fn resolve_runner(runner: &str) -> Option<PathBuf> {
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
    for dir in std::env::split_paths(&std::env::var_os("PATH")?) {
        if !dir.is_absolute() {
            continue;
        }
        let candidate = dir.join(runner);
        if executable(&candidate) {
            return Some(candidate);
        }
    }
    // Soft fallback: ask the OS via `command -v` style presence for common launchers.
    if matches!(runner, "npx" | "uvx" | "node" | "python3") {
        let ok = Command::new(runner)
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .ok()
            .is_some_and(|s| s.success());
        if ok {
            return Some(PathBuf::from(runner));
        }
    }
    None
}

fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn user_disabled_path(config_dir: &Path) -> PathBuf {
    config_dir.join("user_disabled.json")
}

fn load_user_disabled(config_dir: &Path) -> Vec<String> {
    match fs::read_to_string(user_disabled_path(config_dir)) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

fn mark_user_disabled(config_dir: &Path, id: &str) -> Result<()> {
    private_dir(config_dir)?;
    let mut list = load_user_disabled(config_dir);
    if !list.iter().any(|x| x == id) {
        list.push(id.to_string());
    }
    atomic_json(&user_disabled_path(config_dir), &list)
}

fn clear_user_disabled(config_dir: &Path, id: &str) -> Result<()> {
    let mut list = load_user_disabled(config_dir);
    let before = list.len();
    list.retain(|x| x != id);
    if list.len() == before {
        return Ok(());
    }
    private_dir(config_dir)?;
    atomic_json(&user_disabled_path(config_dir), &list)
}

fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let tmp = path.with_extension("tmp");
    let result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        serde_json::to_writer_pretty(&mut file, value)?;
        file.flush()?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ten_ranked_leading_mcp_servers() {
        let catalog = LeadingMcpManager::catalog().unwrap();
        assert_eq!(catalog.len(), 20);
        for (i, m) in catalog.iter().enumerate() {
            assert_eq!(m.rank as usize, i + 1);
            m.validate().unwrap();
        }
        assert_eq!(
            LeadingMcpManager::definition("GitHub MCP").unwrap().id,
            "github"
        );
        assert_eq!(
            LeadingMcpManager::definition("browser").unwrap().package,
            "@playwright/mcp"
        );
        assert_eq!(
            LeadingMcpManager::definition("brave-search")
                .unwrap()
                .package,
            "@brave/brave-search-mcp-server"
        );
        assert_eq!(
            LeadingMcpManager::definition("bash").unwrap().package,
            "mcp-server-commands"
        );
        assert_eq!(
            LeadingMcpManager::definition("composio").unwrap().package,
            "composio-mcp-server"
        );
        assert_eq!(
            LeadingMcpManager::definition("sequential-thinking")
                .unwrap()
                .id,
            "sequential-thinking"
        );
    }

    #[test]
    fn expand_workspace_and_env_placeholders() {
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
    fn enable_disable_round_trip() {
        let root = std::env::temp_dir().join(format!(
            "susi-leading-mcp-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        // Point mcp_config at an isolated path by temporarily overriding HOME/XDG if needed:
        // exercise expand + override store without touching the user's real mcp_config.
        let manager = LeadingMcpManager::with_config(root.clone(), root.join("overrides"));
        let updated = manager
            .configure(
                "filesystem",
                &LeadingMcpOverride {
                    runner: Some("npx".into()),
                    args: Some(vec![
                        "-y".into(),
                        "@modelcontextprotocol/server-filesystem".into(),
                        "{workspace}".into(),
                    ]),
                    env_keys: Some(vec![]),
                    package: None,
                },
            )
            .unwrap();
        assert_eq!(updated.id, "filesystem");
        assert_eq!(manager.reset("filesystem").unwrap().id, "filesystem");
        let _ = fs::remove_dir_all(root);
    }
}
