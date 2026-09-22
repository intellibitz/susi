//! End-to-end Gemini CLI control plane (headless executor + auth readiness).
//!
//! Task lifecycle reuses [`super::AgentManager`]; this module owns doctor/setup
//! checks for API keys, Vertex ADC, and cached `~/.gemini` login. When the
//! operator has `GEMINI_API_KEY` but host settings select `gateway` (e.g. IDE
//! Antigravity), workers isolate via `GEMINI_CLI_HOME` so headless runs use
//! API-key auth.

use anyhow::{bail, Context, Result};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::catalog::{definition, resolve_program, CatalogKind};

pub const AGENT_ID: &str = "gemini-cli";
pub const DOCUMENTATION: &str = "https://google-gemini.github.io/gemini-cli/docs/cli/headless.html";
pub const INSTALL_DOCS: &str = "https://www.npmjs.com/package/@google/gemini-cli";
pub const AUTH_DOCS: &str =
    "https://google-gemini.github.io/gemini-cli/docs/get-started/authentication.html";

/// True when headless Gemini CLI can authenticate without an interactive login prompt.
pub fn credentials_present() -> bool {
    env_credentials_present() || cached_oauth_usable() || gateway_configured()
}

fn env_credentials_present() -> bool {
    for key in ["GEMINI_API_KEY", "GOOGLE_API_KEY"] {
        if !std::env::var(key).unwrap_or_default().trim().is_empty() {
            return true;
        }
    }
    !std::env::var("GOOGLE_APPLICATION_CREDENTIALS")
        .unwrap_or_default()
        .trim()
        .is_empty()
}

fn gateway_configured() -> bool {
    !std::env::var("GOOGLE_GEMINI_BASE_URL")
        .unwrap_or_default()
        .trim()
        .is_empty()
        && selected_auth_type().as_deref() == Some("gateway")
}

fn selected_auth_type() -> Option<String> {
    let path = gemini_settings_path()?;
    let body = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    v.pointer("/security/auth/selectedType")
        .and_then(|x| x.as_str())
        .map(str::to_string)
}

fn gemini_settings_path() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("GEMINI_CLI_HOME") {
        let p = PathBuf::from(home).join(".gemini").join("settings.json");
        if p.is_file() {
            return Some(p);
        }
    }
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let p = home.join(".gemini").join("settings.json");
    p.is_file().then_some(p)
}

fn cached_auth_present() -> bool {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let candidates = [
        home.as_ref()
            .map(|h| h.join(".gemini").join("google_accounts.json")),
        home.as_ref()
            .map(|h| h.join(".gemini").join("settings.json")),
        home.as_ref().map(|h| h.join(".gemini").join(".env")),
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .map(|h| h.join("gemini").join("settings.json")),
    ];
    candidates.into_iter().flatten().any(|p| p.is_file())
}

/// Cached Google login is usable for headless only when selectedType is oauth (not gateway).
fn cached_oauth_usable() -> bool {
    match selected_auth_type().as_deref() {
        Some("oauth-personal") | Some("cloud-shell") | Some("compute-default-credentials") => {
            cached_auth_present()
        }
        Some("gemini-api-key") => env_credentials_present(),
        Some("gateway") => gateway_configured(),
        Some(_) => false,
        None => {
            cached_auth_present() && !matches!(selected_auth_type().as_deref(), Some("gateway"))
        }
    }
}

pub fn model_override() -> Option<String> {
    for key in ["GEMINI_MODEL", "GOOGLE_GENAI_MODEL"] {
        let m = std::env::var(key).unwrap_or_default();
        let trimmed = m.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}

/// Local readiness: `gemini` on PATH + credentials (env or usable cached login).
pub fn doctor() -> Result<String> {
    let def = definition(CatalogKind::Execution, AGENT_ID)?;
    let detail = def.adapter.preflight()?;
    if !credentials_present() {
        let hint = match selected_auth_type().as_deref() {
            Some("gateway") => {
                "host ~/.gemini settings use auth selectedType=gateway (IDE); \
                 for headless set GEMINI_API_KEY (SUSI isolates via GEMINI_CLI_HOME), \
                 or set GOOGLE_GEMINI_BASE_URL for gateway, \
                 or change selectedType to gemini-api-key / oauth-personal"
            }
            _ => {
                "set GEMINI_API_KEY (AI Studio) or Vertex credentials \
                 (GOOGLE_GENAI_USE_VERTEXAI=true + GOOGLE_API_KEY / ADC), \
                 or run `gemini` once interactively to cache oauth-personal login"
            }
        };
        bail!("{hint}; see {AUTH_DOCS}");
    }
    let model = model_override().unwrap_or_else(|| "(CLI default)".into());
    let auth = selected_auth_type().unwrap_or_else(|| "(unset)".into());
    Ok(format!(
        "{detail}; Gemini credentials present; selectedType={auth}; model={model}"
    ))
}

pub fn setup() -> serde_json::Value {
    let binary = resolve_program("gemini");
    json!({
        "agent": AGENT_ID,
        "documentation": DOCUMENTATION,
        "install": INSTALL_DOCS,
        "authentication": AUTH_DOCS,
        "program": "gemini",
        "binary_present": binary.is_some(),
        "binary_path": binary.as_ref().map(|p| p.display().to_string()),
        "credentials_present": credentials_present(),
        "env_credentials_present": env_credentials_present(),
        "cached_auth_present": cached_auth_present(),
        "selected_auth_type": selected_auth_type(),
        "model_override": model_override(),
        "adapter": definition(CatalogKind::Execution, AGENT_ID)
            .map(|d| serde_json::to_value(d.adapter).unwrap_or(json!(null)))
            .unwrap_or(json!(null)),
        "instructions": "1) Install: `npm install -g @google/gemini-cli`\n2) Auth for headless: export GEMINI_API_KEY=… from AI Studio\n   Or Vertex: GOOGLE_GENAI_USE_VERTEXAI=true + GOOGLE_API_KEY / ADC + GOOGLE_CLOUD_PROJECT + GOOGLE_CLOUD_LOCATION\n   Or interactive oauth-personal (not gateway) cached under ~/.gemini\n   Note: IDE gateway selectedType breaks headless — SUSI isolates with GEMINI_CLI_HOME when GEMINI_API_KEY is set\n3) `susi gemini doctor` then `susi gemini run --wait \"…\"`\n4) Also: `susi agents run gemini-cli`\nNo packages or subscriptions are provisioned implicitly."
    })
}

pub fn status() -> serde_json::Value {
    json!({
        "agent": AGENT_ID,
        "documentation": DOCUMENTATION,
        "binary_present": resolve_program("gemini").is_some(),
        "credentials_present": credentials_present(),
        "env_credentials_present": env_credentials_present(),
        "cached_auth_present": cached_auth_present(),
        "selected_auth_type": selected_auth_type(),
        "model_override": model_override(),
    })
}

/// Ensure a workspace-local Gemini home that forces API-key auth for headless workers.
pub fn ensure_api_key_home(workspace: &Path) -> Result<PathBuf> {
    let home = workspace.join(".susi").join("gemini-cli").join("home");
    let gemini = home.join(".gemini");
    std::fs::create_dir_all(&gemini).with_context(|| format!("mkdir {}", gemini.display()))?;
    let settings = gemini.join("settings.json");
    if !settings.exists() {
        std::fs::write(
            &settings,
            r#"{"security":{"auth":{"selectedType":"gemini-api-key"}}}"#,
        )
        .with_context(|| format!("write {}", settings.display()))?;
    }
    Ok(home)
}

/// Apply headless env so IDE `gateway` settings do not break API-key runs.
pub fn apply_headless_env(cmd: &mut Command, workspace: &Path) {
    if std::env::var_os("GEMINI_CLI_HOME").is_some() {
        return;
    }
    if std::env::var("GEMINI_API_KEY")
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        return;
    }
    // Isolate whenever host selectedType is missing/unusable for API-key headless.
    let needs_isolation = match selected_auth_type().as_deref() {
        Some("gemini-api-key") => false,
        Some("oauth-personal") | Some("vertex-ai") | Some("compute-default-credentials") => true,
        Some("gateway") => true,
        _ => true,
    };
    if needs_isolation {
        if let Ok(home) = ensure_api_key_home(workspace) {
            cmd.env("GEMINI_CLI_HOME", home);
        }
    }
    if std::env::var_os("SUSI_PROCESS_BANNER").is_none() {
        cmd.env("SUSI_PROCESS_BANNER", "susi-gemini-cli");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_entry_uses_headless_json_yolo() {
        let def = definition(CatalogKind::Execution, AGENT_ID).unwrap();
        match def.adapter {
            crate::external::Adapter::Command { program, args } => {
                assert_eq!(program, "gemini");
                assert!(args.iter().any(|a| a == "-p"));
                assert!(args.iter().any(|a| a == "{prompt}"));
                assert!(args.iter().any(|a| a == "--output-format"));
                assert!(args.iter().any(|a| a == "json"));
                assert!(args.iter().any(|a| a == "--yolo"));
                assert!(args.iter().any(|a| a == "--skip-trust"));
            }
            other => panic!("expected command adapter, got {other:?}"),
        }
    }

    #[test]
    fn api_key_home_writes_settings() {
        let dir = std::env::temp_dir().join(format!(
            "susi-gemini-home-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let home = ensure_api_key_home(&dir).unwrap();
        let settings = home.join(".gemini").join("settings.json");
        assert!(settings.is_file());
        let body = std::fs::read_to_string(settings).unwrap();
        assert!(body.contains("gemini-api-key"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
