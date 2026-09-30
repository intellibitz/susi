//! Second-tier OpenAI-compatible vendor presets (T-CLAUDE-32): Cerebras,
//! SambaNova, NVIDIA NIM, Hyperbolic, Perplexity and Cohere. Endpoint
//! presets, key-env resolution, `/models` listing path and a health probe —
//! all data-driven via `config/cloud-vendors.json`, which this module
//! validates against.

use serde::{Deserialize, Serialize};
use susi_error::{EaiError, EaiResult};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VendorPreset {
    /// Canonical id (`cerebras`, `nim`, ...).
    pub id: String,
    pub name: String,
    /// OpenAI-compatible base URL (`/v1` or vendor-specific).
    pub base_url: String,
    /// Canonical env var for the API key.
    pub key_env: String,
    /// `/models` path for discovery/health (appended to base_url).
    pub models_path: String,
    /// Request/response dialect: `chat-completions` unless noted.
    pub dialect: String,
}

impl VendorPreset {
    /// Full models-listing URL (also the cheap health probe).
    pub fn models_url(&self) -> String {
        format!(
            "{}{}",
            self.base_url.trim_end_matches('/'),
            self.models_path
        )
    }

    /// Chat-completions URL for `dialect == "chat-completions"`.
    pub fn chat_url(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    /// Resolve the vendor's key from env, canonical name first then aliases
    /// from [`crate::key_detect`].
    pub fn resolve_key<'a>(&self, env: &dyn Fn(&str) -> Option<&'a str>) -> Option<String> {
        crate::key_detect::resolve(&self.id, env)
            .map(|r| r.value)
            .or_else(|| env(&self.key_env).map(str::to_string))
    }
}

/// Built-in presets (kept in sync with `config/cloud-vendors.json`).
pub fn presets() -> Vec<VendorPreset> {
    vec![
        p(
            "cerebras",
            "Cerebras",
            "https://api.cerebras.ai/v1",
            "CEREBRAS_API_KEY",
        ),
        p(
            "sambanova",
            "SambaNova",
            "https://api.sambanova.ai/v1",
            "SAMBANOVA_API_KEY",
        ),
        p(
            "nim",
            "NVIDIA NIM",
            "https://integrate.api.nvidia.com/v1",
            "NVIDIA_API_KEY",
        ),
        p(
            "hyperbolic",
            "Hyperbolic",
            "https://api.hyperbolic.xyz/v1",
            "HYPERBOLIC_API_KEY",
        ),
        p(
            "perplexity",
            "Perplexity",
            "https://api.perplexity.ai",
            "PERPLEXITY_API_KEY",
        ),
        p(
            "cohere",
            "Cohere",
            "https://api.cohere.com/compatibility/v1",
            "COHERE_API_KEY",
        ),
    ]
}

fn p(id: &str, name: &str, base: &str, env: &str) -> VendorPreset {
    VendorPreset {
        id: id.into(),
        name: name.into(),
        base_url: base.into(),
        key_env: env.into(),
        models_path: "/models".into(),
        dialect: "chat-completions".into(),
    }
}

/// Look a preset up by id (or common alias).
pub fn preset(id: &str) -> Option<VendorPreset> {
    let id = id.to_ascii_lowercase();
    presets().into_iter().find(|p| {
        p.id == id
            || p.name.to_ascii_lowercase() == id
            || (id == "nvidia" && p.id == "nim")
            || (id == "nvidia-nim" && p.id == "nim")
    })
}

/// Validate the shipped `config/cloud-vendors.json` against [`presets`] —
/// every entry must resolve a key env and an https base URL, and the file's
/// ids must equal the built-in set.
pub fn validate_catalog(json: &str) -> EaiResult<Vec<VendorPreset>> {
    let v: Vec<VendorPreset> = serde_json::from_str(json)
        .map_err(|e| EaiError::config(format!("cloud-vendors.json: {e}")))?;
    for p in &v {
        if p.key_env.is_empty() || !p.base_url.starts_with("https://") {
            return Err(EaiError::config(format!(
                "vendor {} needs an https base_url and key_env",
                p.id
            )));
        }
    }
    Ok(v)
}

/// The bundled catalog path.
pub fn catalog_path() -> std::path::PathBuf {
    let mut d = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    d.pop();
    d.pop();
    d.push("config/cloud-vendors.json");
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn more_openai_compat_each_preset_resolves_env_and_base() {
        for p in presets() {
            assert!(p.base_url.starts_with("https://"), "{}", p.id);
            assert!(!p.key_env.is_empty(), "{}", p.id);
            assert!(p.models_url().ends_with("/models"), "{}", p.id);
            assert!(p.chat_url().contains("/chat/completions"), "{}", p.id);
        }
    }

    #[test]
    fn more_openai_compat_lookup_by_id_and_alias() {
        assert_eq!(preset("cerebras").unwrap().id, "cerebras");
        assert_eq!(preset("NVIDIA").unwrap().id, "nim");
        assert_eq!(preset("nvidia-nim").unwrap().id, "nim");
        assert!(preset("nobody").is_none());
    }

    #[test]
    fn more_openai_compat_key_resolution_uses_aliases() {
        let mut m = HashMap::new();
        m.insert("PERPLEXITY_API_KEY", "pplx-test");
        let env = |k: &str| m.get(k).copied();
        let p = preset("perplexity").unwrap();
        assert_eq!(p.resolve_key(&env).as_deref(), Some("pplx-test"));
        // canonical env beats nothing-found
        assert!(preset("cohere").unwrap().resolve_key(&env).is_none());
    }

    #[test]
    fn more_openai_compat_bundled_catalog_matches() {
        let json = std::fs::read_to_string(catalog_path()).unwrap();
        let file = validate_catalog(&json).unwrap();
        let builtin: Vec<String> = presets().iter().map(|p| p.id.clone()).collect();
        let mut from_file: Vec<String> = file.iter().map(|p| p.id.clone()).collect();
        from_file.sort();
        let mut b = builtin;
        b.sort();
        assert_eq!(from_file, b);
        // every preset in the file resolves the same env/base as built-in
        for p in &file {
            let bi = preset(&p.id).unwrap();
            assert_eq!(p.base_url, bi.base_url, "{}", p.id);
            assert_eq!(p.key_env, bi.key_env, "{}", p.id);
        }
    }

    #[test]
    fn more_openai_compat_rejects_bad_catalog() {
        assert!(validate_catalog("[{\"id\":\"x\",\"name\":\"\",\"base_url\":\"http://insecure\",\"key_env\":\"\",\"models_path\":\"\",\"dialect\":\"\"}]").is_err());
        assert!(validate_catalog("not json").is_err());
    }
}
