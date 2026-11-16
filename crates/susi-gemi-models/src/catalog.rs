//! Shared catalog mechanics; each manager retains its own lookup and policy.

use std::{fs, path::Path};
use susi_error::{eai_bail as bail, EaiResult, ResultExt};

pub(crate) fn apply_override(target: &mut String, value: Option<String>, allow_empty: bool) {
    if let Some(value) = value.filter(|value| allow_empty || !value.trim().is_empty()) {
        *target = value;
    }
}

pub(crate) fn preferred(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

pub(crate) fn clear_preferred(path: &Path, cleared: &str, absent: &str) -> EaiResult<String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(cleared.into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(absent.into()),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn validate_model(required: [&str; 3], key_env: &str, family: &str) -> EaiResult<()> {
    if required.iter().any(|value| value.trim().is_empty()) {
        bail!("{family} model id, engine, and model must be nonempty");
    }
    if !key_env
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        bail!("api_key_env must name an environment variable");
    }
    Ok(())
}

/// `require_remote_key` preserves coding models' stricter readiness policy:
/// frontier models permit remote endpoints that declare no credential variable.
pub(crate) fn preflight(
    engine: &str,
    key_env: &str,
    require_remote_key: bool,
) -> EaiResult<&'static str> {
    let endpoint = crate::cloud::endpoint_named(engine)
        .with_context(|| format!("inference endpoint '{engine}' is not configured"))?;
    if endpoint.api_base.trim().is_empty() {
        bail!("endpoint '{engine}' has an empty api_base");
    }
    let key_env = if key_env.is_empty() {
        &endpoint.api_key_env
    } else {
        key_env
    };
    let key = crate::cloud::resolve_api_key(key_env, engine);
    let remote = crate::cloud::is_remote_cloud(endpoint.api_base.trim());
    if remote && (require_remote_key || !key_env.is_empty()) && key.is_empty() {
        bail!("set {key_env} (or `susi keys set {engine}`) for cloud access");
    }
    Ok(if key_env.is_empty() || !remote {
        "not required (local)"
    } else {
        "present"
    })
}

pub(crate) fn resolve_endpoint(
    engine: &str,
    protocol: &str,
    key_env: &str,
) -> EaiResult<(susi_config::InferenceEndpointItem, String)> {
    let mut endpoint =
        crate::cloud::endpoint_named(engine).context("endpoint missing after preflight")?;
    if !key_env.is_empty() {
        endpoint.api_key_env = key_env.to_owned();
    }
    if !protocol.is_empty() {
        endpoint.protocol_type = protocol.to_owned();
    }
    let key = crate::cloud::resolve_api_key(&endpoint.api_key_env, engine);
    Ok((endpoint, key))
}
