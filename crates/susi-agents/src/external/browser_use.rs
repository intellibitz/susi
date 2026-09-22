//! End-to-end Browser Use control plane (CLI prompt mode + LLM readiness).
//!
//! Task lifecycle reuses [`super::AgentManager`]. Headless one-shot runs use
//! `browser-use --prompt … --headless`.

use anyhow::{bail, Result};
use serde_json::json;
use std::path::PathBuf;
use std::process::Command;

use super::catalog::{definition, resolve_program, CatalogKind};

pub const AGENT_ID: &str = "browser-use";
pub const DOCUMENTATION: &str = "https://docs.browser-use.com/open-source/browser-use-cli";
pub const INSTALL_DOCS: &str = "https://github.com/browser-use/browser-use";
pub const PACKAGE: &str = "browser-use[cli]";

/// True when Browser Use can resolve an LLM / cloud API key.
pub fn credentials_present() -> bool {
    env_credentials_present() || config_present()
}

fn env_credentials_present() -> bool {
    for key in [
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "GOOGLE_API_KEY",
        "GEMINI_API_KEY",
        "BROWSER_USE_API_KEY",
        "OPENROUTER_API_KEY",
        "DEEPSEEK_API_KEY",
        "AZURE_OPENAI_API_KEY",
    ] {
        if !std::env::var(key).unwrap_or_default().trim().is_empty() {
            return true;
        }
    }
    false
}

fn config_present() -> bool {
    browser_use_home()
        .map(|h| {
            h.join(".env").is_file()
                || h.join("config.json").is_file()
                || h.join("settings.json").is_file()
        })
        .unwrap_or(false)
}

fn browser_use_home() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("BROWSER_USE_HOME") {
        return Some(PathBuf::from(p));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".browser-use"))
}

pub fn model_override() -> Option<String> {
    for key in ["BROWSER_USE_MODEL", "LLM_MODEL", "OPENAI_MODEL"] {
        let m = std::env::var(key).unwrap_or_default();
        let trimmed = m.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}

/// Soft check: Chromium/Chrome available (Playwright or system browser).
pub fn browser_runtime_detail() -> (bool, String) {
    if !std::env::var("BU_CDP_URL")
        .unwrap_or_default()
        .trim()
        .is_empty()
        || !std::env::var("BU_CDP_WS")
            .unwrap_or_default()
            .trim()
            .is_empty()
    {
        return (
            true,
            "CDP endpoint configured (BU_CDP_URL/BU_CDP_WS)".into(),
        );
    }
    for bin in [
        "chromium",
        "chromium-browser",
        "google-chrome",
        "google-chrome-stable",
        "chrome",
    ] {
        if resolve_program(bin).is_some() {
            return (true, format!("{bin} on PATH"));
        }
    }
    // Playwright browsers often live under cache; presence of CLI install helper is soft evidence.
    if resolve_program("browser-use").is_some() {
        return (
            false,
            "no system Chrome/Chromium on PATH — run `browser-use install` or set BU_CDP_URL"
                .into(),
        );
    }
    (false, "browser runtime unknown".into())
}

/// Local readiness: `browser-use` on PATH + LLM credentials.
pub fn doctor() -> Result<String> {
    let def = definition(CatalogKind::Execution, AGENT_ID)?;
    let detail = def.adapter.preflight()?;
    if !credentials_present() {
        bail!(
            "set OPENAI_API_KEY / ANTHROPIC_API_KEY / GOOGLE_API_KEY / BROWSER_USE_API_KEY \
             (model auto-detect uses the first available); see {INSTALL_DOCS}"
        );
    }
    let model = model_override().unwrap_or_else(|| "(auto-detect from API keys)".into());
    let (browser_ok, browser_detail) = browser_runtime_detail();
    let browser_note = if browser_ok {
        browser_detail
    } else {
        format!("{browser_detail} (soft — install may still launch Playwright Chromium)")
    };
    Ok(format!(
        "{detail}; Browser Use credentials present; model={model}; {browser_note}"
    ))
}

pub fn setup() -> serde_json::Value {
    let binary = resolve_program("browser-use");
    let (browser_ok, browser_detail) = browser_runtime_detail();
    json!({
        "agent": AGENT_ID,
        "documentation": DOCUMENTATION,
        "install": INSTALL_DOCS,
        "package": PACKAGE,
        "program": "browser-use",
        "binary_present": binary.is_some(),
        "binary_path": binary.as_ref().map(|p| p.display().to_string()),
        "credentials_present": credentials_present(),
        "env_credentials_present": env_credentials_present(),
        "config_present": config_present(),
        "browser_ok": browser_ok,
        "browser_detail": browser_detail,
        "model_override": model_override(),
        "adapter": definition(CatalogKind::Execution, AGENT_ID)
            .map(|d| serde_json::to_value(d.adapter).unwrap_or(json!(null)))
            .unwrap_or(json!(null)),
        "instructions": "1) Install: `pip install \"browser-use[cli]\"` or `uv tool install browser-use`\n2) Browser: `browser-use install` (Chromium) or set BU_CDP_URL to an existing CDP endpoint\n3) Auth: export OPENAI_API_KEY / ANTHROPIC_API_KEY / GOOGLE_API_KEY / BROWSER_USE_API_KEY\n   Optional: BROWSER_USE_MODEL=gpt-5-mini (or claude-… / gemini-…)\n4) `susi browser-use doctor` then `susi browser-use run --wait \"open example.com and report the title\"`\n5) Also: `susi agents run browser-use`\nHeadless: `browser-use --prompt \"…\" --headless`\nNo packages or subscriptions are provisioned implicitly."
    })
}

pub fn status() -> serde_json::Value {
    let (browser_ok, browser_detail) = browser_runtime_detail();
    json!({
        "agent": AGENT_ID,
        "documentation": DOCUMENTATION,
        "binary_present": resolve_program("browser-use").is_some(),
        "credentials_present": credentials_present(),
        "env_credentials_present": env_credentials_present(),
        "config_present": config_present(),
        "browser_ok": browser_ok,
        "browser_detail": browser_detail,
        "model_override": model_override(),
    })
}

/// Optional one-shot model override for the durable worker.
pub fn apply_model_override(cmd: &mut Command) {
    if let Some(model) = model_override() {
        cmd.arg("--model").arg(model);
    }
    if std::env::var_os("SUSI_PROCESS_BANNER").is_none() {
        cmd.env("SUSI_PROCESS_BANNER", "susi-browser-use");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_entry_uses_prompt_headless() {
        let def = definition(CatalogKind::Execution, AGENT_ID).unwrap();
        match def.adapter {
            crate::external::Adapter::Command { program, args } => {
                assert_eq!(program, "browser-use");
                assert!(args.iter().any(|a| a == "--prompt"));
                assert!(args.iter().any(|a| a == "{prompt}"));
                assert!(args.iter().any(|a| a == "--headless"));
            }
            other => panic!("expected command adapter, got {other:?}"),
        }
    }
}
