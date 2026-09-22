//! AutoGen (AgentChat) E2E management — doctor, setup report, and workspace init.
//!
//! SUSI does not vendor AutoGen; the operator installs `autogen-agentchat` (+
//! `autogen-ext[openai]`) and points `SUSI_AUTOGEN_CONFIG` at an absolute JSON
//! file with `script` or `entrypoint`.

use super::catalog::{definition, Adapter, CatalogKind};
use anyhow::{bail, Context, Result};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const ENGINE_ID: &str = "autogen";
pub const CONFIG_ENV: &str = "SUSI_AUTOGEN_CONFIG";
pub const IMPORT_NAME: &str = "autogen_agentchat";

/// Example agent script written by `susi autogen init` (operator-owned).
pub const EXAMPLE_SCRIPT: &str = r#"#!/usr/bin/env python3
"""Minimal AutoGen AgentChat entry used by SUSI. Replace with your real agents."""
from __future__ import annotations

import asyncio
import json
import os
import sys

from autogen_agentchat.agents import AssistantAgent
from autogen_ext.models.openai import OpenAIChatCompletionClient


async def _run(prompt: str) -> str:
    model = os.environ.get("AUTOGEN_MODEL", os.environ.get("OPENAI_MODEL", "gpt-4o-mini"))
    model_client = OpenAIChatCompletionClient(model=model)
    agent = AssistantAgent(
        "susi_autogen",
        model_client=model_client,
        system_message="You are a helpful coding assistant invoked by SUSI. Be concise.",
    )
    try:
        result = await agent.run(task=prompt)
        texts = []
        for msg in getattr(result, "messages", []) or []:
            content = getattr(msg, "content", None)
            if content is not None:
                texts.append(str(content))
            else:
                texts.append(str(msg))
        return texts[-1] if texts else str(result)
    finally:
        await model_client.close()


def main() -> None:
    prompt = os.environ.get("SUSI_AGENT_PROMPT", "")
    answer = asyncio.run(_run(prompt))
    print(json.dumps({"prompt": prompt, "answer": answer}, ensure_ascii=False), flush=True)


if __name__ == "__main__":
    main()
    sys.stdout.flush()
"#;

#[derive(Debug, Clone)]
pub struct AutoGenDoctor {
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

impl AutoGenDoctor {
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
                "ready — run with: susi autogen run \"your task\""
            } else {
                "install: pip/uv install \"autogen-agentchat\" \"autogen-ext[openai]\"; export OPENAI_API_KEY; then susi autogen init (or point SUSI_AUTOGEN_CONFIG at absolute JSON with script/entrypoint)"
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
        "AZURE_OPENAI_API_KEY",
        "OPENROUTER_API_KEY",
    ] {
        if !std::env::var(key).unwrap_or_default().trim().is_empty() {
            return (true, format!("{key} present"));
        }
    }
    (
        false,
        "set OPENAI_API_KEY (or AZURE_OPENAI_API_KEY / OPENROUTER_API_KEY + base URL for OpenAI-compatible clients)"
            .into(),
    )
}

fn check_config_env() -> (bool, String) {
    let Some(raw) = std::env::var_os(CONFIG_ENV) else {
        return (
            false,
            format!("{CONFIG_ENV} unset — run: susi autogen init"),
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
        bail!("autogen adapter must be kind=python");
    };
    let doctor = AutoGenDoctor::run(python);
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
            "Install: pip install \"autogen-agentchat\" \"autogen-ext[openai]\"   (or: uv add …)",
            "Auth: export OPENAI_API_KEY=…   (Azure: AZURE_OPENAI_API_KEY; OpenRouter: OPENROUTER_API_KEY + OPENAI_BASE_URL)",
            "Optional model: export AUTOGEN_MODEL=gpt-4o-mini",
            "Init workspace scaffold: susi autogen init",
            "Or set SUSI_AUTOGEN_CONFIG to an absolute JSON with {\"script\":\"/abs/path.py\"} or {\"entrypoint\":\"module:callable\"}",
            "Callable/script receives the task via SUSI_AGENT_PROMPT",
            "Doctor: susi autogen doctor",
            "Run: susi autogen run \"your task\"   (add --wait to block)",
            "Lifecycle: susi autogen tasks | status <id> | logs <id> | cancel <id>",
            "Also available under: susi frameworks … autogen"
        ],
    }))
}

/// Write example script + config under `<workspace>/.susi/autogen/`.
pub fn init_workspace(workspace: &Path) -> Result<serde_json::Value> {
    let dir = workspace.join(".susi").join("autogen");
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
            "pip/uv install \"autogen-agentchat\" \"autogen-ext[openai]\"",
            "export OPENAI_API_KEY=…",
            "susi autogen doctor",
            "susi autogen run \"say hello\" --wait"
        ]
    }))
}

/// If `SUSI_AUTOGEN_CONFIG` is unset, bind the workspace scaffold when present.
pub fn bind_workspace_config(workspace: &Path) -> Option<PathBuf> {
    if std::env::var_os(CONFIG_ENV).is_some() {
        return None;
    }
    let config = workspace.join(".susi").join("autogen").join("config.json");
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
        cmd.env("SUSI_PROCESS_BANNER", "susi-autogen");
    }
}

pub fn doctor_or_bail(python: &str) -> Result<String> {
    let d = AutoGenDoctor::run(python);
    if !d.ready {
        bail!(
            "autogen not ready: python={} package={} config={} credentials={} — {}",
            if d.python_ok { "ok" } else { "fail" },
            if d.package_ok { "ok" } else { "fail" },
            if d.config_ok { "ok" } else { "fail" },
            if d.credentials_ok { "ok" } else { "fail" },
            d.to_json()["hint"].as_str().unwrap_or("")
        );
    }
    Ok(format!(
        "python={}; autogen_agentchat={}; config={}; credentials={}",
        d.python_detail, d.package_detail, d.config_detail, d.credentials_detail
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_script_mentions_agentchat() {
        assert!(EXAMPLE_SCRIPT.contains("from autogen_agentchat.agents import"));
        assert!(EXAMPLE_SCRIPT.contains("AssistantAgent"));
        assert!(EXAMPLE_SCRIPT.contains("SUSI_AGENT_PROMPT"));
    }

    #[test]
    fn init_writes_config_and_script() {
        let dir = std::env::temp_dir().join(format!(
            "susi-autogen-init-{}",
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
        let d = AutoGenDoctor::run("python3");
        assert!(!d.config_ok);
        match prev {
            Some(v) => std::env::set_var(CONFIG_ENV, v),
            None => std::env::remove_var(CONFIG_ENV),
        }
    }
}
