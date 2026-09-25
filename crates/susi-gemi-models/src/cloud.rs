//! Cloud env / endpoint metadata for the models tier (no HTTP provider construction).
//!
//! Key storage (`~/.susi/cloud.env`) and inference-endpoint merge live here so the
//! models crate never depends on engines. Provider registry registration stays in
//! `susi-gemi::http_provider`.
//!
//! Vendor env aliases come from the active extension pack
//! (`config/extensions/default/cloud-vendors.json`); host overrides at
//! `~/.susi/extensions/<pack>/cloud-vendors.json`.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use crate::susi_sandbox::extensions::{load_cloud_vendors, CloudVendorEntry};

/// Zero-config cloud secrets: `~/.susi/cloud.env` (KEY=value lines).
/// Shell / process env always wins; this file only fills missing keys so an
/// always-on systemd daemon still sees API keys without editing config.json.
pub fn cloud_env_path() -> PathBuf {
    crate::susi_paths::SusiDirs::config_dir().join("cloud.env")
}

/// Parse a dotenv-style file into key/value pairs (no side effects).
pub fn parse_env_file(content: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || key.contains(char::is_whitespace) {
            continue;
        }
        let mut value = value.trim().to_string();
        if (value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\''))
        {
            value = value[1..value.len().saturating_sub(1)].to_string();
        }
        if value.is_empty() {
            continue;
        }
        out.push((key.to_string(), value));
    }
    out
}

/// Load `~/.susi/cloud.env` into the process environment for any key not
/// already set. Idempotent; safe to call from CLI and daemon boot.
#[allow(unsafe_code)]
pub fn apply_cloud_env_file() {
    let path = cloud_env_path();
    let Ok(content) = std::fs::read_to_string(&path) else {
        return;
    };
    for (key, value) in parse_env_file(&content) {
        match std::env::var(&key) {
            Ok(existing) if !existing.is_empty() => continue,
            _ => {
                // SAFETY: susi owns these vendor key names; we only set when unset.
                unsafe {
                    std::env::set_var(&key, &value);
                }
            }
        }
    }
}

fn generic_vendor_api_key_env(vendor: &str) -> String {
    format!(
        "{}_API_KEY",
        vendor
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .collect::<String>()
            .trim_matches('_')
            .replace("__", "_")
    )
}

fn find_vendor_entry(needle: &str) -> Option<CloudVendorEntry> {
    let lower = needle.to_ascii_lowercase();
    load_cloud_vendors().into_iter().find(|v| {
        v.id.eq_ignore_ascii_case(&lower)
            || v.aliases.iter().any(|a| a.eq_ignore_ascii_case(&lower))
    })
}

/// Map a user-facing vendor name (or raw `FOO_API_KEY`) to the env var name.
pub fn resolve_vendor_env_name(vendor: &str) -> Option<String> {
    let trimmed = vendor.trim();
    if trimmed.is_empty() {
        return None;
    }
    let upper = trimmed.to_ascii_uppercase().replace('-', "_");
    if upper.ends_with("_API_KEY") || upper.ends_with("_KEY") {
        return Some(upper);
    }
    if let Some(entry) = find_vendor_entry(trimmed) {
        return Some(entry.api_key_env);
    }
    // Unknown vendor → `{VENDOR}_API_KEY` so custom OpenAI-compat still works
    // once they also have an endpoint (or use OpenRouter). Protocol-generic
    // escape hatch — not a hardcoded vendor table.
    Some(generic_vendor_api_key_env(trimmed))
}

/// Known vendors the CLI can suggest (id → env var), from the active pack.
pub fn known_cloud_vendors() -> Vec<(String, String)> {
    load_cloud_vendors()
        .into_iter()
        .map(|v| (v.id, v.api_key_env))
        .collect()
}

/// Upsert `KEY=value` in `~/.susi/cloud.env` (chmod 600 on Unix) and apply
/// into the current process. Does **not** register HTTP providers — that stays
/// in the engines crate (`susi_gemi::http_provider::register_api_key`).
#[allow(unsafe_code)]
pub fn register_api_key(vendor: &str, api_key: &str) -> Result<(String, PathBuf), String> {
    let key = api_key.trim();
    if key.is_empty() {
        return Err("API key must not be empty".into());
    }
    let env_name = resolve_vendor_env_name(vendor)
        .ok_or_else(|| "vendor name must not be empty".to_string())?;
    let path = upsert_cloud_env_key(&env_name, key)?;
    // Force into this process even if a stale empty value existed.
    unsafe {
        std::env::set_var(&env_name, key);
    }
    apply_cloud_env_file();
    Ok((env_name, path))
}

/// Remove a vendor key from `~/.susi/cloud.env` and the current process env.
#[allow(unsafe_code)]
pub fn remove_api_key(vendor: &str) -> Result<String, String> {
    let env_name = resolve_vendor_env_name(vendor)
        .ok_or_else(|| "vendor name must not be empty".to_string())?;
    let path = cloud_env_path();
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    let mut kept = Vec::new();
    let mut removed = false;
    for raw in content.lines() {
        let trimmed = raw.trim();
        let check = trimmed.strip_prefix("export ").unwrap_or(trimmed);
        if let Some((k, _)) = check.split_once('=') {
            if k.trim().eq_ignore_ascii_case(&env_name) {
                removed = true;
                continue;
            }
        }
        kept.push(raw.to_string());
    }
    if !removed
        && std::env::var(&env_name)
            .ok()
            .filter(|v| !v.is_empty())
            .is_none()
    {
        return Err(format!("{} was not registered", env_name));
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let body = if kept.is_empty() {
        String::new()
    } else {
        format!("{}\n", kept.join("\n"))
    };
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true).mode(0o600);
        let mut file = options.open(&path).map_err(|e| e.to_string())?;
        file.write_all(body.as_bytes()).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&path, body).map_err(|e| e.to_string())?;
    }
    unsafe {
        std::env::remove_var(&env_name);
    }
    Ok(format!("Removed {} from {}", env_name, path.display()))
}

/// Status of registered cloud keys (env var names only — never values).
pub fn list_api_key_status() -> Vec<(String, String, bool)> {
    let file_keys: HashSet<String> = std::fs::read_to_string(cloud_env_path())
        .ok()
        .map(|c| parse_env_file(&c).into_iter().map(|(k, _)| k).collect())
        .unwrap_or_default();
    known_cloud_vendors()
        .into_iter()
        .map(|(vendor, env)| {
            let present = file_keys.contains(&env)
                || std::env::var(&env).ok().filter(|v| !v.is_empty()).is_some();
            (vendor, env, present)
        })
        .collect()
}

fn upsert_cloud_env_key(env_name: &str, value: &str) -> Result<PathBuf, String> {
    let path = cloud_env_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut lines: Vec<String> = Vec::new();
    let mut replaced = false;
    for raw in existing.lines() {
        let trimmed = raw.trim();
        let check = trimmed.strip_prefix("export ").unwrap_or(trimmed);
        if let Some((k, _)) = check.split_once('=') {
            if k.trim().eq_ignore_ascii_case(env_name) {
                lines.push(format!("{}={}", env_name, value));
                replaced = true;
                continue;
            }
        }
        lines.push(raw.to_string());
    }
    if !replaced {
        lines.push(format!("{}={}", env_name, value));
    }
    let body = format!("{}\n", lines.join("\n"));
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let _ = std::fs::set_permissions(
            path.parent().unwrap_or_else(|| std::path::Path::new(".")),
            std::fs::Permissions::from_mode(0o700),
        );
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true).mode(0o600);
        let mut file = options.open(&path).map_err(|e| e.to_string())?;
        file.write_all(body.as_bytes()).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&path, body).map_err(|e| e.to_string())?;
    }
    Ok(path)
}

/// Bundled defaults ∪ user `inference_endpoints` by name (user wins).
pub fn effective_inference_endpoints_pub(
) -> Vec<crate::susi_sandbox::manager::InferenceEndpointItem> {
    effective_inference_endpoints()
}

/// Bundled defaults ∪ user `inference_endpoints` by name (user wins).
/// Ensures new OpenAI-compat presets (DeepSeek, Kimi, …) appear even when
/// `~/.susi/config.json` still has an older endpoints array.
pub fn effective_inference_endpoints() -> Vec<crate::susi_sandbox::manager::InferenceEndpointItem> {
    let bundled = crate::susi_sandbox::manager::SusiConfig::default()
        .inference_endpoints()
        .endpoints;
    let user = crate::susi_sandbox::manager::SusiConfig::load_global()
        .unwrap_or_default()
        .inference_endpoints()
        .endpoints;
    let mut by_name: BTreeMap<String, crate::susi_sandbox::manager::InferenceEndpointItem> =
        BTreeMap::new();
    for endpoint in bundled {
        by_name.insert(endpoint.name.to_ascii_lowercase(), endpoint);
    }
    for endpoint in user {
        by_name.insert(endpoint.name.to_ascii_lowercase(), endpoint);
    }
    by_name
        .into_values()
        .map(|mut endpoint| {
            remap_retired_endpoint_model(&mut endpoint);
            endpoint
        })
        .collect()
}

/// Host `config.json` often freezes an endpoint's `model` at first seed.
/// When that ID has been retired by the vendor, substitute the current
/// bundled default so registration does not advertise a known-404 name.
fn remap_retired_endpoint_model(
    endpoint: &mut crate::susi_sandbox::manager::InferenceEndpointItem,
) {
    let name = endpoint.name.to_ascii_lowercase();
    let model = endpoint.model.as_str();
    let replacement = match (name.as_str(), model) {
        (
            "groq",
            "llama-3.3-70b-versatile"
            | "llama-3.1-8b-instant"
            | "llama-3.1-70b-versatile"
            | "mixtral-8x7b-32768",
        ) => Some("openai/gpt-oss-20b"),
        (
            "googlegemini" | "gemini" | "google",
            "gemini-2.0-pro" | "gemini-1.5-pro" | "gemini-2.5-pro" | "gemini-2.5-flash"
            | "gemini-2.0-flash" | "gemini-1.5-flash",
        ) => Some("gemini-3.6-flash"),
        _ => None,
    };
    if let Some(next) = replacement {
        endpoint.model = next.to_string();
    }
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// Resolve an API key from env / vendor convention (never logs the value).
pub fn resolve_api_key(api_key_env: &str, endpoint_name: &str) -> String {
    if !api_key_env.is_empty() {
        if let Some(v) = env_nonempty(api_key_env) {
            return v;
        }
    }
    // Pack aliases + primary/alt env names (OpenAI-compat + native APIs).
    if let Some(entry) = find_vendor_entry(endpoint_name) {
        if let Some(v) = env_nonempty(&entry.api_key_env) {
            return v;
        }
        for alt in &entry.api_key_env_alts {
            if let Some(v) = env_nonempty(alt) {
                return v;
            }
        }
    }
    // Any named endpoint: try `{NAME}_API_KEY` (e.g. DeepSeek → DEEPSEEK_API_KEY).
    let generic = generic_vendor_api_key_env(endpoint_name);
    if !generic.is_empty() {
        if let Some(v) = env_nonempty(&generic) {
            return v;
        }
    }
    String::new()
}

/// True for HTTPS remote bases (not localhost). Used to treat any
/// OpenAI-compatible cloud vendor as a cloud provider for routing.
pub fn is_remote_cloud(api_base: &str) -> bool {
    let lower = api_base.to_ascii_lowercase();
    lower.starts_with("https://") && !lower.contains("localhost") && !lower.contains("127.0.0.1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remap_retired_groq_and_gemini_models() {
        let mut groq = crate::susi_sandbox::manager::InferenceEndpointItem {
            name: "Groq".into(),
            api_base: "https://api.groq.com/openai/v1".into(),
            protocol_type: "chat".into(),
            model: "llama-3.3-70b-versatile".into(),
            api_key_env: "GROQ_API_KEY".into(),
            ..Default::default()
        };
        remap_retired_endpoint_model(&mut groq);
        assert_eq!(groq.model, "openai/gpt-oss-20b");

        let mut gemini = crate::susi_sandbox::manager::InferenceEndpointItem {
            name: "GoogleGemini".into(),
            api_base: "https://generativelanguage.googleapis.com/v1beta".into(),
            protocol_type: "gemini".into(),
            model: "gemini-2.5-flash".into(),
            api_key_env: "GEMINI_API_KEY".into(),
            ..Default::default()
        };
        remap_retired_endpoint_model(&mut gemini);
        assert_eq!(gemini.model, "gemini-3.6-flash");
    }

    #[test]
    fn parse_env_file_supports_export_and_comments() {
        let parsed = parse_env_file(
            r#"
# comment
export DEEPSEEK_API_KEY=sk-deep
MOONSHOT_API_KEY="sk-kimi"
MINIMAX_API_KEY='sk-mm'
EMPTY=
INVALID LINE
OPENAI_API_KEY=sk-oai
"#,
        );
        assert_eq!(
            parsed,
            vec![
                ("DEEPSEEK_API_KEY".into(), "sk-deep".into()),
                ("MOONSHOT_API_KEY".into(), "sk-kimi".into()),
                ("MINIMAX_API_KEY".into(), "sk-mm".into()),
                ("OPENAI_API_KEY".into(), "sk-oai".into()),
            ]
        );
    }

    #[test]
    fn resolve_vendor_env_name_maps_aliases() {
        assert_eq!(
            resolve_vendor_env_name("deepseek").as_deref(),
            Some("DEEPSEEK_API_KEY")
        );
        assert_eq!(
            resolve_vendor_env_name("kimi").as_deref(),
            Some("MOONSHOT_API_KEY")
        );
        assert_eq!(
            resolve_vendor_env_name("gemini").as_deref(),
            Some("GEMINI_API_KEY")
        );
        assert_eq!(
            resolve_vendor_env_name("claude").as_deref(),
            Some("ANTHROPIC_API_KEY")
        );
        assert_eq!(
            resolve_vendor_env_name("OPENAI_API_KEY").as_deref(),
            Some("OPENAI_API_KEY")
        );
    }

    #[test]
    fn resolve_vendor_env_name_generic_escape_hatch() {
        assert_eq!(
            resolve_vendor_env_name("acme-cloud").as_deref(),
            Some("ACME_CLOUD_API_KEY")
        );
    }

    #[test]
    fn known_cloud_vendors_come_from_pack() {
        let vendors = known_cloud_vendors();
        assert!(vendors
            .iter()
            .any(|(id, env)| id == "openai" && env == "OPENAI_API_KEY"));
        assert!(vendors.iter().any(|(id, _)| id == "deepseek"));
        assert!(vendors.len() >= 10);
    }

    #[test]
    fn resolve_api_key_reads_env() {
        // SAFETY: test-only env mutation in a single-threaded unit test.
        unsafe {
            std::env::set_var("SUSI_TEST_CLOUD_KEY", "sk-test-123");
        }
        let key = resolve_api_key("SUSI_TEST_CLOUD_KEY", "OpenAI");
        assert_eq!(key, "sk-test-123");
        unsafe {
            std::env::remove_var("SUSI_TEST_CLOUD_KEY");
        }
    }

    #[test]
    fn is_remote_cloud_detects_https_vendors() {
        assert!(is_remote_cloud("https://api.openai.com/v1"));
        assert!(!is_remote_cloud("http://localhost:11434/v1"));
        assert!(!is_remote_cloud("https://127.0.0.1:8443/v1"));
    }
}
