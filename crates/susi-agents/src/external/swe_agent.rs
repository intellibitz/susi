//! End-to-end SWE-agent control plane (CLI executor + LLM/Docker readiness).
//!
//! Task lifecycle reuses [`super::AgentManager`]. Headless local runs use
//! `--problem_statement.text` + `--env.repo.path` + `--actions.apply_patch_locally`.

use anyhow::{bail, Result};
use serde_json::json;
use std::process::Command;

use super::catalog::{definition, resolve_program, CatalogKind};

pub const AGENT_ID: &str = "swe-agent";
pub const DOCUMENTATION: &str = "https://swe-agent.com/latest/usage/hello_world/";
pub const INSTALL_DOCS: &str = "https://swe-agent.com/latest/installation/";
pub const TUTORIAL_DOCS: &str = "https://swe-agent.com/latest/usage/cl_tutorial/";

/// True when SWE-agent can resolve an LM provider key from the environment.
pub fn llm_credentials_present() -> bool {
    for key in [
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "OPENROUTER_API_KEY",
        "DEEPSEEK_API_KEY",
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
        "TOGETHER_API_KEY",
        "GROQ_API_KEY",
    ] {
        if !std::env::var(key).unwrap_or_default().trim().is_empty() {
            return true;
        }
    }
    false
}

pub fn model_override() -> Option<String> {
    for key in [
        "SWE_AGENT_MODEL",
        "AIDER_MODEL",
        "LLM_MODEL",
        "ANTHROPIC_MODEL",
        "OPENAI_MODEL",
    ] {
        let m = std::env::var(key).unwrap_or_default();
        let trimmed = m.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}

pub fn docker_present() -> bool {
    Command::new("docker")
        .args(["info"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Local readiness: `sweagent` on PATH + LLM credentials (+ docker recommended).
pub fn doctor() -> Result<String> {
    let def = definition(CatalogKind::Execution, AGENT_ID)?;
    let detail = def.adapter.preflight()?;
    if !llm_credentials_present() {
        bail!(
            "set ANTHROPIC_API_KEY / OPENAI_API_KEY / OPENROUTER_API_KEY (or another \
             litellm-compatible provider key); optionally SWE_AGENT_MODEL=claude-sonnet-4-20250514; \
             see {TUTORIAL_DOCS}"
        );
    }
    let model = model_override().unwrap_or_else(|| "claude-sonnet-4-20250514 (default)".into());
    let docker = if docker_present() {
        "docker ok"
    } else {
        "docker missing/unavailable — default deployment needs Docker (or configure Modal/cloud)"
    };
    Ok(format!(
        "{detail}; SWE-agent LLM credentials present; model={model}; {docker}"
    ))
}

pub fn setup() -> serde_json::Value {
    let binary = resolve_program("sweagent");
    json!({
        "agent": AGENT_ID,
        "documentation": DOCUMENTATION,
        "install": INSTALL_DOCS,
        "tutorial": TUTORIAL_DOCS,
        "program": "sweagent",
        "binary_present": binary.is_some(),
        "binary_path": binary.as_ref().map(|p| p.display().to_string()),
        "llm_credentials_present": llm_credentials_present(),
        "docker_present": docker_present(),
        "model_override": model_override(),
        "default_model": "claude-sonnet-4-20250514",
        "adapter": definition(CatalogKind::Execution, AGENT_ID)
            .map(|d| serde_json::to_value(d.adapter).unwrap_or(json!(null)))
            .unwrap_or(json!(null)),
        "instructions": "1) Install: `pip install sweagent` (needs Docker for default sandbox)\n2) Export ANTHROPIC_API_KEY (or OPENAI_API_KEY / OPENROUTER_API_KEY)\n   Optional: SWE_AGENT_MODEL=claude-sonnet-4-20250514\n3) `susi swe-agent doctor` then `susi swe-agent run --wait \"fix the failing test\"`\n4) Also: `susi agents run swe-agent`\nHeadless local: problem_statement.text + env.repo.path=workspace + apply_patch_locally\nNote: upstream recommends mini-swe-agent for new work; this manages classic `sweagent` CLI\nNo packages or subscriptions are provisioned implicitly."
    })
}

pub fn status() -> serde_json::Value {
    json!({
        "agent": AGENT_ID,
        "documentation": DOCUMENTATION,
        "binary_present": resolve_program("sweagent").is_some(),
        "llm_credentials_present": llm_credentials_present(),
        "docker_present": docker_present(),
        "model_override": model_override(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_entry_uses_local_repo_text_prompt() {
        let def = definition(CatalogKind::Execution, AGENT_ID).unwrap();
        match def.adapter {
            crate::external::Adapter::Command { program, args } => {
                assert_eq!(program, "sweagent");
                assert_eq!(args.first().map(String::as_str), Some("run"));
                assert!(args.iter().any(|a| a == "--problem_statement.text"));
                assert!(args.iter().any(|a| a == "{prompt}"));
                assert!(args.iter().any(|a| a == "--env.repo.path"));
                assert!(args.iter().any(|a| a == "{workspace}"));
                assert!(args
                    .iter()
                    .any(|a| a == "--actions.apply_patch_locally=true"));
                assert!(args.iter().any(|a| a == "--agent.model.name"));
                assert!(args.iter().any(|a| a == "{model}"));
            }
            other => panic!("expected command adapter, got {other:?}"),
        }
    }
}
