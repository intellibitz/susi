//! End-to-end OpenClaw control plane (`agent exec` + auth readiness).
//!
//! Task lifecycle reuses [`super::AgentManager`]. Headless CI/coding runs use
//! `openclaw agent exec --cwd … --message … --json`.

use anyhow::{bail, Result};
use serde_json::json;
use std::path::PathBuf;
use std::process::Command;

use super::catalog::{definition, resolve_program, CatalogKind};

pub const AGENT_ID: &str = "openclaw";
pub const DOCUMENTATION: &str = "https://docs.openclaw.ai/cli/agent";
pub const INSTALL_DOCS: &str = "https://docs.openclaw.ai/install";
pub const ONBOARD_DOCS: &str = "https://docs.openclaw.ai/start/getting-started";

/// True when OpenClaw can authenticate via env provider keys or onboarded config.
pub fn credentials_present() -> bool {
    env_credentials_present() || config_present()
}

fn env_credentials_present() -> bool {
    for key in [
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "OPENROUTER_API_KEY",
        "DEEPSEEK_API_KEY",
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
        "GROQ_API_KEY",
        "TOGETHER_API_KEY",
        "OPENCLAW_API_KEY",
    ] {
        if !std::env::var(key).unwrap_or_default().trim().is_empty() {
            return true;
        }
    }
    false
}

fn config_present() -> bool {
    openclaw_home()
        .map(|h| {
            h.join("openclaw.json").is_file()
                || h.join("openclaw.json.last-good").is_file()
                || h.join("agents").is_dir()
        })
        .unwrap_or(false)
}

fn openclaw_home() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("OPENCLAW_HOME") {
        return Some(PathBuf::from(p));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".openclaw"))
}

pub fn model_override() -> Option<String> {
    for key in ["OPENCLAW_MODEL", "LLM_MODEL", "AIDER_MODEL"] {
        let m = std::env::var(key).unwrap_or_default();
        let trimmed = m.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}

/// Soft check: OpenClaw requires Node 22.22.3+/24.15+/25.9+.
pub fn node_version_detail() -> (bool, String) {
    match Command::new("node").arg("--version").output() {
        Ok(o) if o.status.success() => {
            let raw = String::from_utf8_lossy(&o.stdout);
            let v = raw.trim().trim_start_matches('v');
            let ok = node_version_ok(v);
            (ok, format!("node {v}"))
        }
        Ok(o) => (false, format!("node --version exit {}", o.status)),
        Err(e) => (false, format!("node missing: {e}")),
    }
}

fn node_version_ok(version: &str) -> bool {
    let parts: Vec<u32> = version
        .split('.')
        .take(3)
        .filter_map(|p| p.parse().ok())
        .collect();
    if parts.len() < 2 {
        return false;
    }
    let major = parts[0];
    let minor = parts[1];
    let patch = parts.get(2).copied().unwrap_or(0);
    match major {
        22 => minor > 22 || (minor == 22 && patch >= 3),
        24 => minor >= 15,
        25 => minor >= 9,
        m if m > 25 => true,
        _ => false,
    }
}

/// Local readiness: `openclaw` on PATH + credentials (env or ~/.openclaw).
pub fn doctor() -> Result<String> {
    let def = definition(CatalogKind::Execution, AGENT_ID)?;
    let detail = def.adapter.preflight()?;
    if !credentials_present() {
        bail!(
            "set OPENAI_API_KEY / ANTHROPIC_API_KEY / OPENROUTER_API_KEY (or another provider key), \
             or run `openclaw onboard` to write ~/.openclaw; see {ONBOARD_DOCS}"
        );
    }
    let (node_ok, node_detail) = node_version_detail();
    let model = model_override().unwrap_or_else(|| "(from OpenClaw config / default)".into());
    let node_note = if node_ok {
        node_detail
    } else {
        format!("{node_detail} — OpenClaw needs Node >=22.22.3 <23, >=24.15, or >=25.9")
    };
    Ok(format!(
        "{detail}; OpenClaw credentials present; model={model}; {node_note}"
    ))
}

pub fn setup() -> serde_json::Value {
    let binary = resolve_program("openclaw");
    let (node_ok, node_detail) = node_version_detail();
    json!({
        "agent": AGENT_ID,
        "documentation": DOCUMENTATION,
        "install": INSTALL_DOCS,
        "onboard": ONBOARD_DOCS,
        "program": "openclaw",
        "binary_present": binary.is_some(),
        "binary_path": binary.as_ref().map(|p| p.display().to_string()),
        "credentials_present": credentials_present(),
        "env_credentials_present": env_credentials_present(),
        "config_present": config_present(),
        "openclaw_home": openclaw_home().map(|p| p.display().to_string()),
        "node_ok": node_ok,
        "node_detail": node_detail,
        "model_override": model_override(),
        "adapter": definition(CatalogKind::Execution, AGENT_ID)
            .map(|d| serde_json::to_value(d.adapter).unwrap_or(json!(null)))
            .unwrap_or(json!(null)),
        "instructions": "1) Install: `npm install -g openclaw@latest` (Node 22.22.3+ / 24.15+ / 25.9+)\n2) Auth: `openclaw onboard`  OR  export OPENAI_API_KEY / ANTHROPIC_API_KEY / OPENROUTER_API_KEY\n   Optional: OPENCLAW_MODEL=provider/model; for env-only CI add --auth-env-only via host adapter override\n3) `susi openclaw doctor` then `susi openclaw run --wait \"…\"`\n4) Also: `susi agents run openclaw`\nHeadless: `openclaw agent exec --cwd <workspace> --message <prompt> --json`\nNo packages or subscriptions are provisioned implicitly."
    })
}

pub fn status() -> serde_json::Value {
    let (node_ok, node_detail) = node_version_detail();
    json!({
        "agent": AGENT_ID,
        "documentation": DOCUMENTATION,
        "binary_present": resolve_program("openclaw").is_some(),
        "credentials_present": credentials_present(),
        "env_credentials_present": env_credentials_present(),
        "config_present": config_present(),
        "node_ok": node_ok,
        "node_detail": node_detail,
        "model_override": model_override(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_entry_uses_agent_exec_json() {
        let def = definition(CatalogKind::Execution, AGENT_ID).unwrap();
        match def.adapter {
            crate::external::Adapter::Command { program, args } => {
                assert_eq!(program, "openclaw");
                assert_eq!(args.first().map(String::as_str), Some("agent"));
                assert_eq!(args.get(1).map(String::as_str), Some("exec"));
                assert!(args.iter().any(|a| a == "--cwd"));
                assert!(args.iter().any(|a| a == "{workspace}"));
                assert!(args.iter().any(|a| a == "--message"));
                assert!(args.iter().any(|a| a == "{prompt}"));
                assert!(args.iter().any(|a| a == "--json"));
            }
            other => panic!("expected command adapter, got {other:?}"),
        }
    }

    #[test]
    fn node_version_gate_matches_openclaw_policy() {
        assert!(!node_version_ok("24.5.0"));
        assert!(node_version_ok("24.15.0"));
        assert!(node_version_ok("22.22.3"));
        assert!(!node_version_ok("22.22.2"));
        assert!(node_version_ok("25.9.0"));
        assert!(!node_version_ok("25.8.0"));
    }
}
