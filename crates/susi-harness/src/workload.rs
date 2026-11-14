//! Per-dispatch access to an organization's workload identity broker.

use std::env;
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::process::Command;

const GATEWAY: &str = "SUSI_WORKLOAD_GATEWAY";
const TOKEN_FILE: &str = "SUSI_WORKLOAD_TOKEN_FILE";
const LIMIT: u64 = 65_536;

pub(crate) fn enabled() -> bool {
    env::var_os(GATEWAY).is_some() || env::var_os(TOKEN_FILE).is_some()
}

fn origin(base: &str) -> Option<&str> {
    let base = base.strip_suffix('/').unwrap_or(base);
    let authority = base.strip_prefix("https://")?;
    if authority.is_empty()
        || !authority
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-:[]".contains(&b))
    {
        return None;
    }
    Some(base)
}

fn post(base: &str, endpoint: &str, body: &[u8]) -> Option<susi_http_transport::HttpCall> {
    let path = PathBuf::from(env::var_os(TOKEN_FILE)?);
    if !path.is_absolute() {
        return None;
    }
    let mut text = String::new();
    File::open(path)
        .ok()?
        .take(LIMIT + 1)
        .read_to_string(&mut text)
        .ok()?;
    let token = text.trim();
    if text.len() as u64 > LIMIT || token.is_empty() || !token.bytes().all(|b| b.is_ascii_graphic())
    {
        return None;
    }
    let auth = format!("Bearer {token}");
    let call = susi_http_transport::http_call_with_body(
        "POST",
        &format!("{base}/v1/workload/{endpoint}"),
        &[
            ("authorization", &auth),
            ("content-type", "application/json"),
        ],
        Some(body),
        10,
        0,
    )
    .ok()?;
    (200..300).contains(&call.status).then_some(call)
}

pub(crate) fn lookup(name: &str) -> Option<String> {
    let base = env::var(GATEWAY).ok()?;
    let base = origin(&base)?;
    let body = serde_json::to_vec(&serde_json::json!({ "name": name })).ok()?;
    post(base, "enroll", b"{}")?;
    let response = post(base, "credential", &body)?.into_bytes(LIMIT).ok()?;
    let mut json: serde_json::Value = serde_json::from_slice(&response).ok()?;
    match json.get_mut("value")?.take() {
        serde_json::Value::String(value)
            if !value.trim().is_empty() && !value.contains(['\r', '\n', '\0']) =>
        {
            Some(value)
        }
        _ => None,
    }
}

/// Workload identity, sibling provider credentials, and operator login state
/// belong to the harness. Only the selected provider receives its credential.
pub(crate) fn clean_environment(command: &mut Command) {
    command.env_clear();
    for name in [
        "PATH",
        "PATHEXT",
        "SystemRoot",
        "WINDIR",
        "ComSpec",
        "TEMP",
        "TMP",
        "TMPDIR",
        "LANG",
        "LC_ALL",
    ] {
        if let Some(value) = env::var_os(name) {
            command.env(name, value);
        }
    }
}

const PLAN_TOKEN: &str = "SUSI_CHATGPT_PLAN_ACCESS_TOKEN";
const CODEX_RULE: &str = "OPENAI_FEDERATION_RULE_ID";
const CODEX_TOKEN_FILE: &str = "OPENAI_IDENTITY_TOKEN_FILE";

/// Either the customer's native ChatGPT workspace federation rule or a
/// delegated plan token for a provider-authorized, self-hosted application.
pub(crate) enum CodexGrant {
    Federation {
        rule: crate::secrets::ApiKey,
        token_file: PathBuf,
    },
    Plan(crate::secrets::ApiKey),
}

impl CodexGrant {
    pub(crate) fn resolve() -> Result<Self, &'static str> {
        if env::var_os(CODEX_TOKEN_FILE).is_none() && env::var_os(CODEX_RULE).is_none() {
            return crate::secrets::lookup(PLAN_TOKEN)
                .map(Self::Plan)
                .ok_or("ChatGPT plan credential unavailable for this workload");
        }
        let error =
            "Codex federation requires its own absolute identity token file and an approved rule";
        let file = PathBuf::from(env::var_os(CODEX_TOKEN_FILE).ok_or(error)?);
        let broker_file = PathBuf::from(env::var_os(TOKEN_FILE).ok_or(error)?);
        if !file.is_absolute()
            || !file.is_file()
            || file.canonicalize().map_err(|_| error)?
                == broker_file.canonicalize().map_err(|_| error)?
        {
            return Err(error);
        }
        let rule = crate::secrets::lookup(CODEX_RULE).ok_or(error)?;
        Ok(Self::Federation {
            rule,
            token_file: file,
        })
    }

    pub(crate) fn configure_args(&self, args: &mut Vec<String>) {
        if matches!(self, Self::Plan(_)) {
            for setting in [
                r#"model_provider="openai_chatgpt_plan""#,
                r#"model_providers.openai_chatgpt_plan.name="ChatGPT plan""#,
                r#"model_providers.openai_chatgpt_plan.base_url="https://api.openai.com/v1""#,
                r#"model_providers.openai_chatgpt_plan.env_key="SUSI_CHATGPT_PLAN_ACCESS_TOKEN""#,
                r#"model_providers.openai_chatgpt_plan.wire_api="responses""#,
                "model_providers.openai_chatgpt_plan.requires_openai_auth=false",
                "model_providers.openai_chatgpt_plan.supports_websockets=false",
            ] {
                args.extend(["-c".into(), setting.into()]);
            }
        }
    }

    pub(crate) fn configure_env(&self, command: &mut Command) {
        match self {
            Self::Federation { rule, token_file } => {
                command
                    .env(CODEX_RULE, rule.expose())
                    .env(CODEX_TOKEN_FILE, token_file);
            }
            Self::Plan(token) => {
                command.env(PLAN_TOKEN, token.expose());
            }
        }
    }

    pub(crate) fn value(&self) -> &str {
        match self {
            Self::Federation { rule, .. } => rule.expose(),
            Self::Plan(token) => token.expose(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workload_origin_is_an_https_authority() {
        assert_eq!(
            origin("https://example.org:444/"),
            Some("https://example.org:444")
        );
        for bad in [
            "http://127.0.0.1",
            "https://",
            "https://example.org?next=x",
            "https://u:p@example.org",
            "https://example.org/#fragment",
            "https://example.org\\evil",
        ] {
            assert_eq!(origin(bad), None);
        }
    }
}
