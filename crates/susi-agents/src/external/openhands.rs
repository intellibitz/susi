//! End-to-end OpenHands control plane (CLI executor + LLM env readiness).
//!
//! Task lifecycle reuses [`super::AgentManager`]; this module owns doctor/setup
//! checks that the generic command adapter cannot express (LLM_* env wiring).

use anyhow::{bail, Result};
use serde_json::json;

use super::catalog::{definition, resolve_program, CatalogKind};

pub const AGENT_ID: &str = "openhands";
pub const DOCUMENTATION: &str = "https://docs.openhands.dev/openhands/usage/cli/headless";
pub const INSTALL_DOCS: &str = "https://docs.openhands.dev/openhands/usage/cli/installation";

/// True when OpenHands can resolve a model API key from the environment
/// (with `--override-with-envs`) or from the vendor settings file.
pub fn llm_credentials_present() -> bool {
    for key in [
        "LLM_API_KEY",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "OPENROUTER_API_KEY",
        "DEEPSEEK_API_KEY",
    ] {
        if !std::env::var(key).unwrap_or_default().trim().is_empty() {
            return true;
        }
    }
    dirs_settings_present()
}

fn dirs_settings_present() -> bool {
    // OpenHands persists interactive setup under ~/.openhands/settings.json
    // (or XDG config). Presence is evidence of prior setup, not of a valid key.
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let candidates = [
        home.as_ref()
            .map(|h| h.join(".openhands").join("settings.json")),
        std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .map(|h| h.join("openhands").join("settings.json")),
    ];
    candidates.into_iter().flatten().any(|p| p.is_file())
}

pub fn llm_model() -> Option<String> {
    let m = std::env::var("LLM_MODEL").unwrap_or_default();
    let trimmed = m.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub fn llm_base_url() -> Option<String> {
    let u = std::env::var("LLM_BASE_URL").unwrap_or_default();
    let trimmed = u.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Local readiness: binary on PATH + LLM credentials (env or settings).
pub fn doctor() -> Result<String> {
    let def = definition(CatalogKind::Execution, AGENT_ID)?;
    let detail = def.adapter.preflight()?;
    if !llm_credentials_present() {
        bail!(
            "set LLM_API_KEY (and usually LLM_MODEL) for headless `--override-with-envs`, \
             or run `openhands` once interactively to write ~/.openhands/settings.json; \
             OpenRouter users can export OPENROUTER_API_KEY + LLM_BASE_URL=https://openrouter.ai/api/v1"
        );
    }
    let model = llm_model().unwrap_or_else(|| "(from settings or default)".into());
    Ok(format!("{detail}; LLM credentials present; model={model}"))
}

pub fn setup() -> serde_json::Value {
    let binary = resolve_program("openhands");
    json!({
        "agent": AGENT_ID,
        "documentation": DOCUMENTATION,
        "install": INSTALL_DOCS,
        "program": "openhands",
        "binary_present": binary.is_some(),
        "binary_path": binary.as_ref().map(|p| p.display().to_string()),
        "llm_credentials_present": llm_credentials_present(),
        "llm_model": llm_model(),
        "llm_base_url": llm_base_url(),
        "framework": {
            "id": "openhands-runtime",
            "cli": "susi frameworks",
            "config_env": "SUSI_OPENHANDS_RUNTIME_CONFIG",
            "note": "Python runtime/engine path is distinct from the CLI executor"
        },
        "adapter": definition(CatalogKind::Execution, AGENT_ID)
            .map(|d| serde_json::to_value(d.adapter).unwrap_or(json!(null)))
            .unwrap_or(json!(null)),
        "instructions": "1) Install CLI: `uv tool install openhands` or `pip install openhands` (see install docs)\n2) Export LLM_MODEL + LLM_API_KEY (and optional LLM_BASE_URL for OpenRouter/local)\n   Example OpenRouter: LLM_MODEL=openrouter/anthropic/claude-sonnet-4 LLM_API_KEY=$OPENROUTER_API_KEY LLM_BASE_URL=https://openrouter.ai/api/v1\n3) `susi openhands doctor` then `susi openhands run --wait \"…\"`\n4) Optional Python runtime graph: `susi frameworks setup openhands-runtime`\nNo packages or subscriptions are provisioned implicitly."
    })
}

pub fn status() -> serde_json::Value {
    json!({
        "agent": AGENT_ID,
        "documentation": DOCUMENTATION,
        "binary_present": resolve_program("openhands").is_some(),
        "llm_credentials_present": llm_credentials_present(),
        "llm_model": llm_model(),
        "llm_base_url": llm_base_url(),
        "settings_file_present": dirs_settings_present(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_entry_uses_headless_json_override() {
        let def = definition(CatalogKind::Execution, AGENT_ID).unwrap();
        match def.adapter {
            crate::external::Adapter::Command { program, args } => {
                assert_eq!(program, "openhands");
                assert!(args.iter().any(|a| a == "--headless"));
                assert!(args.iter().any(|a| a == "--json"));
                assert!(args.iter().any(|a| a == "--override-with-envs"));
                assert!(args.iter().any(|a| a == "--exit-without-confirmation"));
                assert!(args.iter().any(|a| a == "-t"));
                assert!(args.iter().any(|a| a == "{prompt}"));
            }
            other => panic!("expected command adapter, got {other:?}"),
        }
    }
}
