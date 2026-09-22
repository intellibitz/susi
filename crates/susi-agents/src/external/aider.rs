//! End-to-end Aider control plane (headless scripting + LLM env readiness).
//!
//! Task lifecycle reuses [`super::AgentManager`]; this module owns doctor/setup
//! checks for provider API keys that the generic command adapter cannot express.

use anyhow::{bail, Result};
use serde_json::json;

use super::catalog::{definition, resolve_program, CatalogKind};

pub const AGENT_ID: &str = "aider";
pub const DOCUMENTATION: &str = "https://aider.chat/docs/scripting.html";
pub const INSTALL_DOCS: &str = "https://aider.chat/docs/install.html";
pub const OPTIONS_DOCS: &str = "https://aider.chat/docs/config/options.html";

/// True when Aider can resolve an LLM provider key from the environment.
pub fn llm_credentials_present() -> bool {
    for key in [
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "OPENROUTER_API_KEY",
        "DEEPSEEK_API_KEY",
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
        "AZURE_API_KEY",
        "COHERE_API_KEY",
        "GROQ_API_KEY",
        "TOGETHER_API_KEY",
        "AIDER_OPENAI_API_KEY",
        "AIDER_ANTHROPIC_API_KEY",
    ] {
        if !std::env::var(key).unwrap_or_default().trim().is_empty() {
            return true;
        }
    }
    // `aider --api-key provider=key` style via AIDER_API_KEY is uncommon; also
    // accept a committed/.env-style config presence as soft evidence of prior setup.
    config_file_present()
}

fn config_file_present() -> bool {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let cwd = std::env::current_dir().ok();
    let candidates = [
        cwd.as_ref().map(|c| c.join(".aider.conf.yml")),
        cwd.as_ref().map(|c| c.join(".env")),
        home.as_ref().map(|h| h.join(".aider.conf.yml")),
        home.as_ref().map(|h| h.join(".env")),
    ];
    candidates.into_iter().flatten().any(|p| p.is_file())
}

pub fn model_override() -> Option<String> {
    for key in ["AIDER_MODEL", "OPENAI_MODEL"] {
        let m = std::env::var(key).unwrap_or_default();
        let trimmed = m.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}

/// Local readiness: `aider` on PATH + LLM credentials (env or config).
pub fn doctor() -> Result<String> {
    let def = definition(CatalogKind::Execution, AGENT_ID)?;
    let detail = def.adapter.preflight()?;
    if !llm_credentials_present() {
        bail!(
            "set OPENAI_API_KEY / ANTHROPIC_API_KEY / OPENROUTER_API_KEY (or another \
             provider key Aider accepts), optionally AIDER_MODEL; \
             or place keys in .aider.conf.yml / .env; see {OPTIONS_DOCS}"
        );
    }
    let model = model_override().unwrap_or_else(|| "(Aider default / config)".into());
    Ok(format!(
        "{detail}; Aider LLM credentials present; model={model}"
    ))
}

pub fn setup() -> serde_json::Value {
    let binary = resolve_program("aider");
    json!({
        "agent": AGENT_ID,
        "documentation": DOCUMENTATION,
        "install": INSTALL_DOCS,
        "options": OPTIONS_DOCS,
        "program": "aider",
        "binary_present": binary.is_some(),
        "binary_path": binary.as_ref().map(|p| p.display().to_string()),
        "llm_credentials_present": llm_credentials_present(),
        "config_file_present": config_file_present(),
        "model_override": model_override(),
        "adapter": definition(CatalogKind::Execution, AGENT_ID)
            .map(|d| serde_json::to_value(d.adapter).unwrap_or(json!(null)))
            .unwrap_or(json!(null)),
        "instructions": "1) Install: `python -m pip install aider-chat` (or uv/pipx)\n2) Export a provider key: OPENAI_API_KEY / ANTHROPIC_API_KEY / OPENROUTER_API_KEY\n   Optional: AIDER_MODEL=openrouter/anthropic/claude-sonnet-4 (or openai/…)\n3) `susi aider doctor` then `susi aider run --wait \"…\"`\n4) Also: `susi agents run aider`\nHeadless flags: --message + --yes-always (no interactive confirmations)\nNo packages or subscriptions are provisioned implicitly."
    })
}

pub fn status() -> serde_json::Value {
    json!({
        "agent": AGENT_ID,
        "documentation": DOCUMENTATION,
        "binary_present": resolve_program("aider").is_some(),
        "llm_credentials_present": llm_credentials_present(),
        "config_file_present": config_file_present(),
        "model_override": model_override(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_entry_uses_scripting_message_yes() {
        let def = definition(CatalogKind::Execution, AGENT_ID).unwrap();
        match def.adapter {
            crate::external::Adapter::Command { program, args } => {
                assert_eq!(program, "aider");
                assert!(args.iter().any(|a| a == "--message"));
                assert!(args.iter().any(|a| a == "{prompt}"));
                assert!(args.iter().any(|a| a == "--yes-always"));
                assert!(args.iter().any(|a| a == "--no-pretty"));
                assert!(args.iter().any(|a| a == "--no-fancy-input"));
            }
            other => panic!("expected command adapter, got {other:?}"),
        }
    }
}
