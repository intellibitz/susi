//! Hugging Face smolagents E2E management — doctor, setup report, and workspace init.
//!
//! SUSI does not vendor smolagents; the operator installs `smolagents` and points
//! `SUSI_SMOLAGENTS_CONFIG` at an absolute JSON file with `script` or `entrypoint`.

use super::catalog::{definition, Adapter, CatalogKind};
use anyhow::{bail, Context, Result};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const ENGINE_ID: &str = "smolagents";
pub const CONFIG_ENV: &str = "SUSI_SMOLAGENTS_CONFIG";
pub const IMPORT_NAME: &str = "smolagents";

/// Example agent script written by `susi smolagents init` (operator-owned).
pub const EXAMPLE_SCRIPT: &str = r#"#!/usr/bin/env python3
"""Minimal Hugging Face smolagents entry used by SUSI. Replace with your real agent."""
from __future__ import annotations

import json
import os
import sys

from smolagents import CodeAgent


def _model():
    model_id = os.environ.get(
        "SMOLAGENTS_MODEL",
        os.environ.get("OPENAI_MODEL", "gpt-4o-mini"),
    )
    if os.environ.get("OPENAI_API_KEY") or os.environ.get("OPENROUTER_API_KEY"):
        from smolagents import OpenAIServerModel

        kwargs = {"model_id": model_id}
        if base := os.environ.get("OPENAI_BASE_URL"):
            kwargs["api_base"] = base
        if key := os.environ.get("OPENAI_API_KEY") or os.environ.get("OPENROUTER_API_KEY"):
            kwargs["api_key"] = key
        return OpenAIServerModel(**kwargs)
    from smolagents import InferenceClientModel

    return InferenceClientModel(
        model_id=os.environ.get(
            "SMOLAGENTS_MODEL",
            "Qwen/Qwen2.5-Coder-32B-Instruct",
        )
    )


def main() -> None:
    prompt = os.environ.get("SUSI_AGENT_PROMPT", "")
    agent = CodeAgent(tools=[], model=_model(), add_base_tools=False)
    output = agent.run(prompt)
    print(json.dumps({"prompt": prompt, "answer": str(output)}, ensure_ascii=False), flush=True)


if __name__ == "__main__":
    main()
    sys.stdout.flush()
"#;

#[derive(Debug, Clone)]
pub struct SmolAgentsDoctor {
    pub python_ok: bool,
    pub python_detail: String,
    pub package_ok: bool,
    pub package_detail: String,
    pub config_ok: bool,
    pub config_detail: String,
    pub credentials_ok: bool,
    pub credentials_detail: String,
    pub ready: bool,
}

impl SmolAgentsDoctor {
    pub fn run(python: &str) -> Self {
        let (python_ok, python_detail) = check_python(python);
        let (package_ok, package_detail) = if python_ok {
            check_import(python, IMPORT_NAME)
        } else {
            (false, "python unavailable".into())
        };
        let (config_ok, config_detail) = check_config_env();
        let (credentials_ok, credentials_detail) = check_credentials();
        let ready = python_ok && package_ok && config_ok && credentials_ok;
        Self {
            python_ok,
            python_detail,
            package_ok,
            package_detail,
            config_ok,
            config_detail,
            credentials_ok,
            credentials_detail,
            ready,
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        json!({
            "engine": ENGINE_ID,
            "ready": self.ready,
            "python": { "ok": self.python_ok, "detail": self.python_detail },
            "package": { "ok": self.package_ok, "detail": self.package_detail },
            "config": { "ok": self.config_ok, "detail": self.config_detail },
            "credentials": { "ok": self.credentials_ok, "detail": self.credentials_detail },
            "config_env": CONFIG_ENV,
            "hint": if self.ready {
                "ready — run with: susi smolagents run \"your task\""
            } else {
                "install: pip/uv install smolagents; export OPENAI_API_KEY or HF_TOKEN; then susi smolagents init (or point SUSI_SMOLAGENTS_CONFIG at absolute JSON with script/entrypoint)"
            }
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

fn check_credentials() -> (bool, String) {
    for key in [
        "OPENAI_API_KEY",
        "OPENROUTER_API_KEY",
        "HF_TOKEN",
        "HUGGINGFACEHUB_API_TOKEN",
        "ANTHROPIC_API_KEY",
    ] {
        if !std::env::var(key).unwrap_or_default().trim().is_empty() {
            return (true, format!("{key} present"));
        }
    }
    (
        false,
        "set OPENAI_API_KEY (OpenAI-compatible) or HF_TOKEN (InferenceClientModel)".into(),
    )
}

fn check_config_env() -> (bool, String) {
    let Some(raw) = std::env::var_os(CONFIG_ENV) else {
        return (
            false,
            format!("{CONFIG_ENV} unset — run: susi smolagents init"),
        );
    };
    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        return (false, format!("{CONFIG_ENV} must be an absolute path"));
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

pub fn setup_report(adapter: &Adapter) -> Result<serde_json::Value> {
    let def = definition(CatalogKind::Framework, ENGINE_ID)?;
    let Adapter::Python {
        python,
        import_name,
        config_env,
    } = adapter
    else {
        bail!("smolagents adapter must be kind=python");
    };
    let doctor = SmolAgentsDoctor::run(python);
    Ok(json!({
        "engine": ENGINE_ID,
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
            "Install: pip install smolagents   (or: uv add smolagents)",
            "Auth: export OPENAI_API_KEY=… for OpenAIServerModel, or HF_TOKEN for InferenceClientModel",
            "Optional: SMOLAGENTS_MODEL=gpt-4o-mini  (or a HF model id)",
            "Init workspace scaffold: susi smolagents init",
            "Or set SUSI_SMOLAGENTS_CONFIG to an absolute JSON with {\"script\":\"/abs/path.py\"} or {\"entrypoint\":\"module:callable\"}",
            "Callable/script receives the task via SUSI_AGENT_PROMPT",
            "Doctor: susi smolagents doctor",
            "Run: susi smolagents run \"your task\"   (add --wait to block)",
            "Lifecycle: susi smolagents tasks | status <id> | logs <id> | cancel <id>",
            "Also available under: susi frameworks … smolagents"
        ],
    }))
}

/// Write example script + config under `<workspace>/.susi/smolagents/`.
pub fn init_workspace(workspace: &Path) -> Result<serde_json::Value> {
    let dir = workspace.join(".susi").join("smolagents");
    std::fs::create_dir_all(&dir).with_context(|| format!("mkdir {}", dir.display()))?;
    let script = dir.join("agent.py");
    let config = dir.join("config.json");
    if !script.exists() {
        std::fs::write(&script, EXAMPLE_SCRIPT)
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
        "engine": ENGINE_ID,
        "script": script_abs,
        "config": config_abs,
        "export": format!("export {CONFIG_ENV}={}", config_abs.display()),
        "next": [
            format!("export {CONFIG_ENV}={}", config_abs.display()),
            "pip/uv install smolagents",
            "export OPENAI_API_KEY=…   # or HF_TOKEN=…",
            "susi smolagents doctor",
            "susi smolagents run \"say hello\" --wait"
        ]
    }))
}

/// If `SUSI_SMOLAGENTS_CONFIG` is unset, bind the workspace scaffold when present.
pub fn bind_workspace_config(workspace: &Path) -> Option<PathBuf> {
    if std::env::var_os(CONFIG_ENV).is_some() {
        return None;
    }
    let config = workspace
        .join(".susi")
        .join("smolagents")
        .join("config.json");
    if config.is_file() {
        let abs = config.canonicalize().unwrap_or(config);
        std::env::set_var(CONFIG_ENV, &abs);
        Some(abs)
    } else {
        None
    }
}

pub fn ensure_process_banner(cmd: &mut Command) {
    if std::env::var_os("SUSI_PROCESS_BANNER").is_none() {
        cmd.env("SUSI_PROCESS_BANNER", "susi-smolagents");
    }
}

pub fn doctor_or_bail(python: &str) -> Result<String> {
    let d = SmolAgentsDoctor::run(python);
    if !d.ready {
        bail!(
            "smolagents not ready: python={} package={} config={} credentials={} — {}",
            if d.python_ok { "ok" } else { "fail" },
            if d.package_ok { "ok" } else { "fail" },
            if d.config_ok { "ok" } else { "fail" },
            if d.credentials_ok { "ok" } else { "fail" },
            d.to_json()["hint"].as_str().unwrap_or("")
        );
    }
    Ok(format!(
        "python={}; smolagents={}; config={}; credentials={}",
        d.python_detail, d.package_detail, d.config_detail, d.credentials_detail
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_script_mentions_smolagents() {
        assert!(EXAMPLE_SCRIPT.contains("from smolagents import CodeAgent"));
        assert!(EXAMPLE_SCRIPT.contains("OpenAIServerModel"));
        assert!(EXAMPLE_SCRIPT.contains("SUSI_AGENT_PROMPT"));
    }

    #[test]
    fn init_writes_config_and_script() {
        let dir = std::env::temp_dir().join(format!(
            "susi-smolagents-init-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let report = init_workspace(&dir).unwrap();
        let config = PathBuf::from(report["config"].as_str().unwrap());
        let script = PathBuf::from(report["script"].as_str().unwrap());
        assert!(config.is_file());
        assert!(script.is_file());
        let body: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
        assert!(PathBuf::from(body["script"].as_str().unwrap()).is_absolute());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn doctor_reports_missing_config() {
        let prev = std::env::var_os(CONFIG_ENV);
        std::env::remove_var(CONFIG_ENV);
        let d = SmolAgentsDoctor::run("python3");
        assert!(!d.config_ok);
        match prev {
            Some(v) => std::env::set_var(CONFIG_ENV, v),
            None => std::env::remove_var(CONFIG_ENV),
        }
    }
}
