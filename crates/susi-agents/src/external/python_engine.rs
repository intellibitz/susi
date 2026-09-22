//! Shared E2E helpers for Python agent-engine adapters (doctor / setup / init / bind).

use super::catalog::{definition, Adapter, CatalogKind};
use anyhow::{bail, Context, Result};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy)]
pub struct EngineProfile {
    pub engine_id: &'static str,
    pub config_env: &'static str,
    pub import_name: &'static str,
    pub package_hint: &'static str,
    pub documentation: &'static str,
    pub scaffold_subdir: &'static str,
    pub example_script: &'static str,
    pub example_filename: &'static str,
    pub credential_envs: &'static [&'static str],
    pub credential_hint: &'static str,
    pub process_banner: &'static str,
    pub cli_name: &'static str,
}

#[derive(Debug, Clone)]
pub struct EngineDoctor {
    pub engine_id: String,
    pub python_ok: bool,
    pub python_detail: String,
    pub package_ok: bool,
    pub package_detail: String,
    pub config_ok: bool,
    pub config_detail: String,
    pub credentials_ok: bool,
    pub credentials_detail: String,
    pub ready: bool,
    pub config_env: String,
    pub hint: String,
}

impl EngineDoctor {
    pub fn run(profile: &EngineProfile, python: &str) -> Self {
        let (python_ok, python_detail) = check_python(python);
        let (package_ok, package_detail) = if python_ok {
            check_import(python, profile.import_name)
        } else {
            (false, "python unavailable".into())
        };
        let (config_ok, config_detail) = check_config_env(profile);
        let (credentials_ok, credentials_detail) = check_credentials(profile);
        let ready = python_ok && package_ok && config_ok && credentials_ok;
        let hint = if ready {
            format!(
                "ready — run with: susi {} run \"your task\"",
                profile.cli_name
            )
        } else {
            format!(
                "install: {}; {}; then susi {} init (or point {} at absolute JSON with script/entrypoint)",
                profile.package_hint,
                profile.credential_hint,
                profile.cli_name,
                profile.config_env
            )
        };
        Self {
            engine_id: profile.engine_id.into(),
            python_ok,
            python_detail,
            package_ok,
            package_detail,
            config_ok,
            config_detail,
            credentials_ok,
            credentials_detail,
            ready,
            config_env: profile.config_env.into(),
            hint,
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        json!({
            "engine": self.engine_id,
            "ready": self.ready,
            "python": { "ok": self.python_ok, "detail": self.python_detail },
            "package": { "ok": self.package_ok, "detail": self.package_detail },
            "config": { "ok": self.config_ok, "detail": self.config_detail },
            "credentials": { "ok": self.credentials_ok, "detail": self.credentials_detail },
            "config_env": self.config_env,
            "hint": self.hint,
        })
    }
}

fn check_python(python: &str) -> (bool, String) {
    match Command::new(python).arg("--version").output() {
        Ok(o) if o.status.success() => {
            let v = String::from_utf8_lossy(&o.stdout);
            let e = String::from_utf8_lossy(&o.stderr);
            (true, format!("{}{}", v.trim(), e.trim()))
        }
        Ok(o) => (false, format!("exit {}", o.status)),
        Err(e) => (false, e.to_string()),
    }
}

fn check_import(python: &str, module: &str) -> (bool, String) {
    let code = format!("import {module}; print(getattr({module}, '__version__', 'ok'))");
    match Command::new(python).args(["-c", &code]).output() {
        Ok(o) if o.status.success() => {
            (true, String::from_utf8_lossy(&o.stdout).trim().to_string())
        }
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            (
                false,
                format!(
                    "import {module} failed: {}",
                    err.lines().last().unwrap_or("unknown")
                ),
            )
        }
        Err(e) => (false, e.to_string()),
    }
}

fn check_credentials(profile: &EngineProfile) -> (bool, String) {
    if profile.credential_envs.is_empty() {
        return (true, "no API key required for local/stdlib entry".into());
    }
    for key in profile.credential_envs {
        if !std::env::var(key).unwrap_or_default().trim().is_empty() {
            return (true, format!("{key} present"));
        }
    }
    (false, profile.credential_hint.into())
}

fn check_config_env(profile: &EngineProfile) -> (bool, String) {
    let Some(raw) = std::env::var_os(profile.config_env) else {
        return (
            false,
            format!(
                "{} unset — run: susi {} init",
                profile.config_env, profile.cli_name
            ),
        );
    };
    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        return (
            false,
            format!("{} must be an absolute path", profile.config_env),
        );
    }
    if !path.is_file() {
        return (false, format!("config file missing: {}", path.display()));
    }
    match std::fs::read_to_string(&path) {
        Ok(body) => match serde_json::from_str::<serde_json::Value>(&body) {
            Ok(v) => {
                let has_script = v.get("script").and_then(|x| x.as_str()).is_some();
                let has_entry = v.get("entrypoint").and_then(|x| x.as_str()).is_some();
                if has_script || has_entry {
                    (true, path.display().to_string())
                } else {
                    (
                        false,
                        "config needs \"script\" (absolute) or \"entrypoint\" (module:callable)"
                            .into(),
                    )
                }
            }
            Err(e) => (false, format!("invalid JSON: {e}")),
        },
        Err(e) => (false, e.to_string()),
    }
}

pub fn setup_report(profile: &EngineProfile, adapter: &Adapter) -> Result<serde_json::Value> {
    let def = definition(CatalogKind::Framework, profile.engine_id)?;
    let Adapter::Python {
        python,
        import_name,
        config_env,
    } = adapter
    else {
        bail!("{} adapter must be kind=python", profile.engine_id);
    };
    let doctor = EngineDoctor::run(profile, python);
    Ok(json!({
        "engine": profile.engine_id,
        "name": def.name,
        "peer_name": def.peer_name,
        "documentation": def.documentation,
        "adapter": {
            "kind": "python",
            "python": python,
            "import_name": import_name,
            "config_env": config_env,
        },
        "doctor": doctor.to_json(),
        "instructions": [
            format!("Install: {}", profile.package_hint),
            format!("Auth: {}", profile.credential_hint),
            format!("Init workspace scaffold: susi {} init", profile.cli_name),
            format!(
                "Or set {} to an absolute JSON with {{\"script\":\"/abs/path.py\"}} or {{\"entrypoint\":\"module:callable\"}}",
                profile.config_env
            ),
            "Callable/script receives the task via SUSI_AGENT_PROMPT",
            format!("Doctor: susi {} doctor", profile.cli_name),
            format!(
                "Run: susi {} run \"your task\"   (add --wait to block)",
                profile.cli_name
            ),
            format!(
                "Lifecycle: susi {} tasks | status <id> | logs <id> | cancel <id>",
                profile.cli_name
            ),
            format!(
                "Also available under: susi frameworks … {}",
                profile.engine_id
            ),
        ],
    }))
}

pub fn init_workspace(profile: &EngineProfile, workspace: &Path) -> Result<serde_json::Value> {
    let dir = workspace.join(".susi").join(profile.scaffold_subdir);
    std::fs::create_dir_all(&dir).with_context(|| format!("mkdir {}", dir.display()))?;
    let script = dir.join(profile.example_filename);
    let config = dir.join("config.json");
    if !script.exists() {
        std::fs::write(&script, profile.example_script)
            .with_context(|| format!("write {}", script.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script, perms)?;
        }
    }
    let script_abs = script.canonicalize().unwrap_or_else(|_| script.clone());
    let body = json!({ "script": script_abs.to_string_lossy() });
    std::fs::write(&config, serde_json::to_vec_pretty(&body)?)
        .with_context(|| format!("write {}", config.display()))?;
    let config_abs = config.canonicalize().unwrap_or_else(|_| config.clone());
    Ok(json!({
        "engine": profile.engine_id,
        "script": script_abs,
        "config": config_abs,
        "export": format!("export {}={}", profile.config_env, config_abs.display()),
        "next": [
            format!("export {}={}", profile.config_env, config_abs.display()),
            profile.package_hint,
            profile.credential_hint,
            format!("susi {} doctor", profile.cli_name),
            format!("susi {} run \"say hello\" --wait", profile.cli_name),
        ]
    }))
}

pub fn bind_workspace_config(profile: &EngineProfile, workspace: &Path) -> Option<PathBuf> {
    if std::env::var_os(profile.config_env).is_some() {
        return None;
    }
    let config = workspace
        .join(".susi")
        .join(profile.scaffold_subdir)
        .join("config.json");
    if config.is_file() {
        let abs = config.canonicalize().unwrap_or(config);
        std::env::set_var(profile.config_env, &abs);
        Some(abs)
    } else {
        None
    }
}

pub fn ensure_process_banner(profile: &EngineProfile, cmd: &mut Command) {
    if std::env::var_os("SUSI_PROCESS_BANNER").is_none() {
        cmd.env("SUSI_PROCESS_BANNER", profile.process_banner);
    }
}

pub fn doctor_or_bail(profile: &EngineProfile, python: &str) -> Result<String> {
    let d = EngineDoctor::run(profile, python);
    if !d.ready {
        bail!(
            "{} not ready: python={} package={} config={} credentials={} — {}",
            profile.engine_id,
            if d.python_ok { "ok" } else { "fail" },
            if d.package_ok { "ok" } else { "fail" },
            if d.config_ok { "ok" } else { "fail" },
            if d.credentials_ok { "ok" } else { "fail" },
            d.hint
        );
    }
    Ok(format!(
        "python={}; {}={}; config={}; credentials={}",
        d.python_detail,
        profile.import_name,
        d.package_detail,
        d.config_detail,
        d.credentials_detail
    ))
}
