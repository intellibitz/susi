//! End-to-end OpenRouter control plane (keys, routing preference, model pin).
//!
//! Catalog ranks are editorial. Paid probe / live `/models` live in
//! `susi_gemi::openrouter_ext` so this crate never depends on engines HTTP.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::cloud::{
    apply_cloud_env_file, effective_inference_endpoints_pub, list_api_key_status, resolve_api_key,
};

pub const VENDOR_ID: &str = "openrouter";
pub const ENGINE_NAME: &str = "OpenRouter";
pub const API_BASE: &str = "https://openrouter.ai/api/v1";
pub const DEFAULT_MODEL: &str = "openrouter/auto";
pub const DOCUMENTATION: &str = "https://openrouter.ai/docs/quickstart";
pub const API_KEY_ENV: &str = "OPENROUTER_API_KEY";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenRouterRoute {
    pub id: String,
    pub name: String,
    pub rank: u8,
    pub model: String,
    #[serde(default)]
    pub documentation: String,
}

#[derive(Clone)]
pub struct OpenRouterManager {
    root: PathBuf,
}

impl OpenRouterManager {
    pub fn new() -> Result<Self> {
        Ok(Self {
            root: susi_paths::SusiDirs::config_dir().join("openrouter"),
        })
    }

    pub fn with_root(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn catalog() -> Result<Vec<OpenRouterRoute>> {
        Ok(susi_sandbox::extensions::load_json_or_bundled(
            "openrouter-models.json",
            include_str!("../../../config/openrouter-models.json"),
        ))
    }

    pub fn definition(id: &str) -> Result<OpenRouterRoute> {
        Self::catalog()?
            .into_iter()
            .find(|r| r.id == id || r.model == id || r.name.eq_ignore_ascii_case(id))
            .with_context(|| format!("unknown OpenRouter route {id}; use `susi openrouter list`"))
    }

    fn preferred_model_path(&self) -> PathBuf {
        self.root.join("preferred_model.txt")
    }

    /// Model id pinned for the OpenRouter endpoint (empty → bundled default).
    pub fn preferred_model(&self) -> Option<String> {
        fs::read_to_string(self.preferred_model_path())
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    pub fn effective_model(&self) -> String {
        self.preferred_model()
            .unwrap_or_else(|| DEFAULT_MODEL.to_string())
    }

    pub fn key_present(&self) -> bool {
        apply_cloud_env_file();
        !resolve_api_key(API_KEY_ENV, ENGINE_NAME).is_empty()
    }

    pub fn endpoint_configured(&self) -> bool {
        effective_inference_endpoints_pub()
            .iter()
            .any(|e| e.name.eq_ignore_ascii_case(ENGINE_NAME) && !e.api_base.trim().is_empty())
    }

    /// Local readiness only — does not call OpenRouter.
    pub fn doctor(&self) -> Result<String> {
        apply_cloud_env_file();
        if !self.endpoint_configured() {
            bail!("OpenRouter inference endpoint missing from config (api_base required)");
        }
        if !self.key_present() {
            bail!("set {API_KEY_ENV} (or `susi keys set openrouter`) for cloud access");
        }
        Ok(format!(
            "OpenRouter ready; model {}; credentials present",
            self.effective_model()
        ))
    }

    pub fn setup(&self) -> serde_json::Value {
        let key_set = self.key_present();
        serde_json::json!({
            "vendor": VENDOR_ID,
            "engine": ENGINE_NAME,
            "api_base": API_BASE,
            "api_key_env": API_KEY_ENV,
            "documentation": DOCUMENTATION,
            "default_model": DEFAULT_MODEL,
            "preferred_model": self.preferred_model(),
            "key_present": key_set,
            "endpoint_configured": self.endpoint_configured(),
            "instructions": "1) Create a key at https://openrouter.ai/keys\n2) `susi keys set openrouter` (or export OPENROUTER_API_KEY)\n3) `susi openrouter doctor` then optional `susi openrouter probe`\n4) `susi openrouter prefer` to pin routing; `susi openrouter prefer <model>` to pin a route id (e.g. anthropic/claude-sonnet-4)\n5) Optional: `susi openrouter models --live` lists provider model ids. No subscriptions are provisioned implicitly."
        })
    }

    pub fn status(&self) -> serde_json::Value {
        apply_cloud_env_file();
        let vendors = list_api_key_status();
        let key_present = vendors
            .iter()
            .find(|(id, _, _)| id == VENDOR_ID)
            .map(|(_, _, p)| *p)
            .unwrap_or_else(|| self.key_present());
        serde_json::json!({
            "vendor": VENDOR_ID,
            "engine": ENGINE_NAME,
            "api_base": API_BASE,
            "api_key_env": API_KEY_ENV,
            "key_present": key_present,
            "endpoint_configured": self.endpoint_configured(),
            "preferred_model": self.preferred_model(),
            "effective_model": self.effective_model(),
            "documentation": DOCUMENTATION,
        })
    }

    /// Pin OpenRouter as preferred cloud; optionally pin a model/route id.
    pub fn prefer(&self, model: Option<&str>) -> Result<String> {
        self.doctor()?;
        private_dir(&self.root)?;
        if let Some(m) = model.map(str::trim).filter(|s| !s.is_empty()) {
            // Accept catalog id or raw OpenRouter model slug.
            let model_id = match Self::definition(m) {
                Ok(route) => route.model,
                Err(_) => {
                    validate_model_slug(m)?;
                    m.to_string()
                }
            };
            fs::write(self.preferred_model_path(), &model_id)?;
            Ok(format!(
                "OpenRouter preferred; model pinned to {model_id}. Clear with `susi openrouter prefer --clear-model`."
            ))
        } else {
            Ok(
                "OpenRouter credentials OK — set cloud preference with `susi keys prefer openrouter`."
                    .to_string(),
            )
        }
    }

    pub fn clear_preferred_model(&self) -> Result<String> {
        match fs::remove_file(self.preferred_model_path()) {
            Ok(()) => Ok("cleared OpenRouter preferred model (using openrouter/auto)".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok("no OpenRouter preferred model was set".into())
            }
            Err(e) => Err(e.into()),
        }
    }

    pub fn resolve_api_key(&self) -> String {
        apply_cloud_env_file();
        resolve_api_key(API_KEY_ENV, ENGINE_NAME)
    }
}

fn validate_model_slug(slug: &str) -> Result<()> {
    if slug.is_empty() || slug.contains("..") || slug.contains(' ') {
        bail!("invalid OpenRouter model id");
    }
    if slug.len() > 200 {
        bail!("OpenRouter model id too long");
    }
    Ok(())
}

fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

/// Attribution headers OpenRouter expects for app rankings / some models.
pub fn attribution_headers() -> (String, String) {
    let cfg = susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
    let referer = cfg
        .get::<String>("openrouter_http_referer")
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "https://github.com/intellibitz/susi".to_string());
    let title = cfg
        .get::<String>("openrouter_app_title")
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "SUSI".to_string());
    (referer, title)
}

pub fn is_openrouter_base(api_base: &str) -> bool {
    api_base.to_ascii_lowercase().contains("openrouter.ai")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_catalog_includes_auto() {
        let cat = OpenRouterManager::catalog().unwrap();
        assert!(cat.iter().any(|r| r.model == "openrouter/auto"));
        assert!(cat.len() >= 5);
    }

    #[test]
    fn validate_model_slug_rejects_traversal() {
        assert!(validate_model_slug("../evil").is_err());
        assert!(validate_model_slug("anthropic/claude-sonnet-4").is_ok());
    }
}
