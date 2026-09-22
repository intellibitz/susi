//! End-to-end DeerFlow control plane (`deerflow` headless CLI + config readiness).
//!
//! Task lifecycle reuses [`super::AgentManager`]. Headless runs use
//! `deerflow --json "{prompt}"` (embedded DeerFlowClient — no Gateway required).

use anyhow::{bail, Result};
use serde_json::json;
use std::path::PathBuf;
use std::process::Command;

use super::catalog::{definition, resolve_program, CatalogKind};

pub const AGENT_ID: &str = "deerflow";
pub const DOCUMENTATION: &str = "https://github.com/bytedance/deer-flow";
pub const INSTALL_DOCS: &str = "https://github.com/bytedance/deer-flow/blob/main/Install.md";
pub const TUI_DOCS: &str = "https://github.com/bytedance/deer-flow/blob/main/backend/docs/TUI.md";
pub const PACKAGE: &str = "deerflow-harness";

pub fn credentials_present() -> bool {
    for key in [
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "OPENROUTER_API_KEY",
        "VOLCENGINE_API_KEY",
        "DEEPSEEK_API_KEY",
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
    ] {
        if !std::env::var(key).unwrap_or_default().trim().is_empty() {
            return true;
        }
    }
    false
}

/// Resolve DeerFlow config.yaml from env (absolute path preferred).
pub fn resolve_config_path() -> Option<PathBuf> {
    if let Some(raw) = std::env::var_os("DEER_FLOW_CONFIG_PATH") {
        let p = PathBuf::from(raw);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some(root) = std::env::var_os("DEER_FLOW_PROJECT_ROOT") {
        let p = PathBuf::from(root).join("config.yaml");
        if p.is_file() {
            return Some(p);
        }
    }
    let cwd = std::env::current_dir().ok()?;
    let p = cwd.join("config.yaml");
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

pub fn config_present() -> bool {
    resolve_config_path().is_some()
}

/// Local readiness: `deerflow` on PATH + config.yaml + model API credentials.
pub fn doctor() -> Result<String> {
    let def = definition(CatalogKind::Execution, AGENT_ID)?;
    let detail = def.adapter.preflight()?;
    if !config_present() {
        bail!(
            "DeerFlow config.yaml missing — run `susi deerflow init`, or set \
             DEER_FLOW_CONFIG_PATH / DEER_FLOW_PROJECT_ROOT (clone + `make setup`); see {INSTALL_DOCS}"
        );
    }
    if !credentials_present() {
        bail!(
            "set OPENAI_API_KEY (or ANTHROPIC_API_KEY / OPENROUTER_API_KEY / VOLCENGINE_API_KEY / …) \
             matching models in config.yaml; see {DOCUMENTATION}"
        );
    }
    let cfg = resolve_config_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "(unknown)".into());
    Ok(format!(
        "{detail}; DeerFlow config present ({cfg}); credentials present"
    ))
}

pub fn setup() -> serde_json::Value {
    let binary = resolve_program("deerflow");
    json!({
        "agent": AGENT_ID,
        "documentation": DOCUMENTATION,
        "install": INSTALL_DOCS,
        "tui_docs": TUI_DOCS,
        "package": PACKAGE,
        "program": "deerflow",
        "binary_present": binary.is_some(),
        "binary_path": binary.as_ref().map(|p| p.display().to_string()),
        "config_present": config_present(),
        "config_path": resolve_config_path().map(|p| p.display().to_string()),
        "credentials_present": credentials_present(),
        "deer_flow_home": std::env::var_os("DEER_FLOW_HOME").map(|p| PathBuf::from(p).display().to_string()),
        "deer_flow_project_root": std::env::var_os("DEER_FLOW_PROJECT_ROOT").map(|p| PathBuf::from(p).display().to_string()),
        "adapter": definition(CatalogKind::Execution, AGENT_ID)
            .map(|d| serde_json::to_value(d.adapter).unwrap_or(json!(null)))
            .unwrap_or(json!(null)),
        "instructions": [
            "1) Install CLI: `uv pip install deerflow-harness`  (optional TUI: `uv pip install 'deerflow-harness[tui]'`)",
            "2a) Quick scaffold: `susi deerflow init` then export DEER_FLOW_CONFIG_PATH / DEER_FLOW_HOME as printed",
            "2b) Full harness: clone https://github.com/bytedance/deer-flow → `make setup` (or Install.md)",
            "3) Auth: export OPENAI_API_KEY=… (or keys referenced by config.yaml models)",
            "4) `susi deerflow doctor` then `susi deerflow run --wait \"summarize this repo\"`",
            "5) Also: `susi agents run deerflow`",
            "Headless: `deerflow --json \"…\"`  (or `deerflow --print \"…\"`)",
            "No packages or subscriptions are provisioned implicitly."
        ],
    })
}

pub fn status() -> serde_json::Value {
    json!({
        "agent": AGENT_ID,
        "documentation": DOCUMENTATION,
        "binary_present": resolve_program("deerflow").is_some(),
        "config_present": config_present(),
        "config_path": resolve_config_path().map(|p| p.display().to_string()),
        "credentials_present": credentials_present(),
    })
}

/// Propagate DeerFlow env into a detached worker command.
pub fn apply_process_env(cmd: &mut Command) {
    if std::env::var_os("SUSI_PROCESS_BANNER").is_none() {
        cmd.env("SUSI_PROCESS_BANNER", "susi-deerflow");
    }
    for key in [
        "DEER_FLOW_CONFIG_PATH",
        "DEER_FLOW_HOME",
        "DEER_FLOW_PROJECT_ROOT",
        "DEER_FLOW_SKILLS_PATH",
    ] {
        if let Ok(v) = std::env::var(key) {
            cmd.env(key, v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_entry_uses_json_prompt() {
        let def = definition(CatalogKind::Execution, AGENT_ID).unwrap();
        match def.adapter {
            crate::external::Adapter::Command { program, args } => {
                assert_eq!(program, "deerflow");
                assert!(args.iter().any(|a| a == "--json"));
                assert!(args.iter().any(|a| a == "{prompt}"));
            }
            other => panic!("expected command adapter, got {other:?}"),
        }
    }
}
