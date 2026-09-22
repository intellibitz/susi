//! End-to-end OpenViking control plane (`ov` client + server readiness).
//!
//! Task lifecycle reuses [`super::AgentManager`]. Headless retrieval uses
//! `ov find "{prompt}" -o json` against a configured OpenViking endpoint.

use anyhow::{bail, Result};
use serde_json::json;
use std::path::PathBuf;
use std::process::Command;

use super::catalog::{definition, resolve_program, CatalogKind};

pub const AGENT_ID: &str = "openviking";
pub const DOCUMENTATION: &str = "https://docs.openviking.ai/en/getting-started/05-cli-setup";
pub const INSTALL_DOCS: &str = "https://docs.openviking.ai/en/getting-started/01-quick-start";
pub const SERVER_DOCS: &str = "https://docs.openviking.ai/en/getting-started/01-quick-start";

/// True when the client can authenticate (API key env or saved ovcli.conf).
pub fn credentials_present() -> bool {
    env_api_key_present() || client_config_present()
}

fn env_api_key_present() -> bool {
    for key in ["OPENVIKING_API_KEY", "OV_API_KEY", "VIKING_API_KEY"] {
        if !std::env::var(key).unwrap_or_default().trim().is_empty() {
            return true;
        }
    }
    false
}

fn openviking_home() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("OPENVIKING_HOME") {
        return Some(PathBuf::from(p));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".openviking"))
}

fn client_config_present() -> bool {
    openviking_home()
        .map(|h| {
            h.join("ovcli.conf").is_file()
                || h.join("ov.conf").is_file()
                || std::fs::read_dir(&h)
                    .ok()
                    .into_iter()
                    .flatten()
                    .filter_map(|e| e.ok())
                    .any(|e| e.file_name().to_string_lossy().starts_with("ovcli.conf."))
        })
        .unwrap_or(false)
}

fn server_config_present() -> bool {
    openviking_home()
        .map(|h| h.join("ov.conf").is_file())
        .unwrap_or(false)
}

pub fn health_ok() -> (bool, String) {
    let Some(ov) = resolve_program("ov") else {
        return (false, "ov missing".into());
    };
    match Command::new(ov)
        .args(["health", "-o", "json"])
        .stdin(std::process::Stdio::null())
        .output()
    {
        Ok(o) if o.status.success() => {
            let body = String::from_utf8_lossy(&o.stdout);
            (true, body.lines().next().unwrap_or("ok").trim().to_string())
        }
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            (
                false,
                format!(
                    "ov health failed: {}",
                    err.lines().last().unwrap_or(&o.status.to_string())
                ),
            )
        }
        Err(e) => (false, e.to_string()),
    }
}

/// Local readiness: `ov` on PATH + client config/API key (+ soft health).
pub fn doctor() -> Result<String> {
    let def = definition(CatalogKind::Execution, AGENT_ID)?;
    let detail = def.adapter.preflight()?;
    if !credentials_present() {
        bail!(
            "configure the OpenViking client: `ov config` or `susi openviking init`, \
             or export OPENVIKING_API_KEY for OpenViking Service; see {DOCUMENTATION}"
        );
    }
    let server = resolve_program("openviking-server").is_some();
    let (healthy, health_detail) = health_ok();
    Ok(format!(
        "{detail}; OpenViking client config present; openviking-server={}; health={}",
        if server {
            "on PATH"
        } else {
            "missing (optional for cloud)"
        },
        if healthy {
            format!("ok ({health_detail})")
        } else {
            format!("unreachable ({health_detail})")
        }
    ))
}

pub fn setup() -> serde_json::Value {
    let ov = resolve_program("ov");
    let server = resolve_program("openviking-server");
    let (healthy, health_detail) = if ov.is_some() {
        health_ok()
    } else {
        (false, "ov missing".into())
    };
    json!({
        "agent": AGENT_ID,
        "documentation": DOCUMENTATION,
        "install": INSTALL_DOCS,
        "server_docs": SERVER_DOCS,
        "program": "ov",
        "binary_present": ov.is_some(),
        "binary_path": ov.as_ref().map(|p| p.display().to_string()),
        "server_present": server.is_some(),
        "server_path": server.as_ref().map(|p| p.display().to_string()),
        "credentials_present": credentials_present(),
        "env_api_key_present": env_api_key_present(),
        "client_config_present": client_config_present(),
        "server_config_present": server_config_present(),
        "openviking_home": openviking_home().map(|p| p.display().to_string()),
        "health_ok": healthy,
        "health_detail": health_detail,
        "adapter": definition(CatalogKind::Execution, AGENT_ID)
            .map(|d| serde_json::to_value(d.adapter).unwrap_or(json!(null)))
            .unwrap_or(json!(null)),
        "instructions": [
            "1) Client: `npm i -g @openviking/cli`  (or: pip/uv install openviking)",
            "2a) Cloud: export OPENVIKING_API_KEY=… then `printf '%s' \"$OPENVIKING_API_KEY\" | ov config add ov-service --name prod --api-key-stdin --activate -o json`",
            "2b) Local server: `pip install openviking` → `openviking-server init` → `openviking-server` → `susi openviking init`",
            "3) Language (required once): `ov language en`",
            "4) `susi openviking doctor` then `susi openviking run --wait \"what is openviking\"`",
            "5) Also: `susi agents run openviking`",
            "Headless retrieval: `ov find \"…\" -o json`",
            "No packages or subscriptions are provisioned implicitly."
        ],
    })
}

pub fn status() -> serde_json::Value {
    let ov = resolve_program("ov");
    let (healthy, health_detail) = if ov.is_some() {
        health_ok()
    } else {
        (false, "ov missing".into())
    };
    json!({
        "agent": AGENT_ID,
        "documentation": DOCUMENTATION,
        "binary_present": ov.is_some(),
        "server_present": resolve_program("openviking-server").is_some(),
        "credentials_present": credentials_present(),
        "client_config_present": client_config_present(),
        "server_config_present": server_config_present(),
        "health_ok": healthy,
        "health_detail": health_detail,
    })
}

/// Ensure durable workers carry a recognizable process banner.
pub fn ensure_process_banner(cmd: &mut Command) {
    if std::env::var_os("SUSI_PROCESS_BANNER").is_none() {
        cmd.env("SUSI_PROCESS_BANNER", "susi-openviking");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_entry_uses_find_json() {
        let def = definition(CatalogKind::Execution, AGENT_ID).unwrap();
        match def.adapter {
            crate::external::Adapter::Command { program, args } => {
                assert_eq!(program, "ov");
                assert_eq!(args.first().map(String::as_str), Some("find"));
                assert!(args.iter().any(|a| a == "{prompt}"));
                assert!(args.iter().any(|a| a == "-o"));
                assert!(args.iter().any(|a| a == "json"));
            }
            other => panic!("expected command adapter, got {other:?}"),
        }
    }
}
