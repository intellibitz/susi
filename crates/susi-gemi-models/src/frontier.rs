//! End-to-end top frontier model control plane (planning / tool calling / long-horizon).
//!
//! Catalog ranks are editorial industry consensus. Paid/local probe lives in
//! `susi_gemi::frontier_ext` so this crate never depends on engines HTTP.

use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::fs;
use susi_error::EaiResult as Result;

use crate::catalog;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrontierVariant {
    pub id: String,
    pub model: String,
    #[serde(default)]
    pub engine: String,
    #[serde(default)]
    pub protocol_type: String,
    #[serde(default)]
    pub api_key_env: String,
}

impl FrontierVariant {
    fn matches(&self, needle: &str) -> bool {
        self.id.eq_ignore_ascii_case(needle) || self.model.eq_ignore_ascii_case(needle)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrontierDefinition {
    pub id: String,
    pub name: String,
    pub rank: u8,
    pub documentation: String,
    #[serde(default)]
    pub family: String,
    pub engine: String,
    /// Provider / local API model id (overridable via host configure or variant).
    pub model: String,
    #[serde(default)]
    pub protocol_type: String,
    #[serde(default)]
    pub api_key_env: String,
    #[serde(default)]
    pub variants: Vec<FrontierVariant>,
    #[serde(default)]
    pub notes: String,
}

crate::catalog::cloud_model_controls!(
    FrontierManager,
    FrontierDefinition,
    FrontierOverride,
    FrontierEndpoint,
    "frontier"
);

crate::catalog::local_catalog!(
    FrontierManager,
    with_config,
    FrontierDefinition,
    "frontier-models",
    "frontier-models.json",
    "unknown frontier model {id}; use `susi frontier list`",
    |model, id| {
        let needle = id.trim();
        model.id.eq_ignore_ascii_case(needle)
            || model.name.eq_ignore_ascii_case(needle)
            || model.model.eq_ignore_ascii_case(needle)
            || model.variants.iter().any(|variant| variant.matches(needle))
    }
);

impl FrontierManager {
    pub fn effective(&self, id: &str) -> Result<FrontierDefinition> {
        let mut def = Self::definition(id)?;
        if let Some(variant) = def.variants.iter().find(|v| v.matches(id.trim())) {
            def.model = variant.model.clone();
            if !variant.engine.is_empty() {
                def.engine = variant.engine.clone();
            }
            if !variant.protocol_type.is_empty() {
                def.protocol_type = variant.protocol_type.clone();
            }
            // Allow clearing api_key_env for local variants (explicit empty string).
            def.api_key_env = variant.api_key_env.clone();
        }
        def.with_stored_override(&self.root)
    }

    pub fn configure(&self, id: &str, over: &FrontierOverride) -> Result<FrontierDefinition> {
        let def = Self::definition(id)?;
        crate::susi_config::write_json_override(&self.root, &def.id, over)?;
        self.effective(&def.id)
    }

    pub fn preferred(&self) -> Option<String> {
        crate::catalog::preferred(&crate::catalog::preferred_path(&self.root))
    }

    /// Local readiness only — does not call a paid model.
    pub fn preflight(&self, id: &str) -> Result<String> {
        let def = self.effective(id)?;
        let credentials = catalog::preflight(&def.engine, &def.api_key_env, false)?;
        Ok(format!(
            "endpoint {} ready; model id {}; credentials {}",
            def.engine, def.model, credentials
        ))
    }

    pub fn prefer(&self, id: &str) -> Result<String> {
        let def = self.effective(id)?;
        self.preflight(id)?;
        crate::susi_config::create_private_dir(&self.root)?;
        // Persist variant/engine pin when the lookup resolved a non-default model id.
        let base = Self::definition(&def.id)?;
        if base.model != def.model || base.engine != def.engine {
            let _ = self.configure(
                &def.id,
                &FrontierOverride {
                    engine: Some(def.engine.clone()),
                    model: Some(def.model.clone()),
                    protocol_type: Some(def.protocol_type.clone()),
                    api_key_env: Some(def.api_key_env.clone()),
                    ..Default::default()
                },
            )?;
        }
        crate::susi_config::atomic_write_bytes(
            &crate::catalog::preferred_path(&self.root),
            def.id.as_bytes(),
        )?;
        crate::set_selected_model_override(&def.model)?;
        Ok(format!(
            "preferred frontier model set to {} (api id {})",
            def.id, def.model
        ))
    }

    pub fn clear_preferred(&self) -> Result<String> {
        crate::catalog::clear_preferred(
            &crate::catalog::preferred_path(&self.root),
            "cleared preferred frontier model",
            "no preferred frontier model was set",
        )
    }

    pub fn setup(&self, id: &str) -> Result<serde_json::Value> {
        let def = self.effective(id)?;
        let ready = self.preflight(&def.id);
        Ok(serde_json::json!({
            "model": def,
            "preferred": self.preferred(),
            "prerequisites_present": ready.is_ok(),
            "detail": match &ready { Ok(s) => s.clone(), Err(e) => e.to_string() },
            "instructions": format!(
                "1) Set credentials: `susi keys set {}` (or export {})\n\
                 2) For Llama local: install Ollama and `ollama pull {}` (or `susi openweight pull llama-3`)\n\
                 3) `susi frontier doctor {id}` then optional `susi frontier probe {id}` (paid/local)\n\
                 4) `susi frontier prefer {id}` to pin routing\n\
                 Variants (e.g. gpt-4o, deepseek-v3, llama-3.1-cloud) accepted by id.",
                def.engine.to_ascii_lowercase().replace(' ', "-"),
                if def.api_key_env.is_empty() { "n/a (local)".into() } else { def.api_key_env.clone() },
                def.model
            )
        }))
    }

    pub fn status(&self) -> serde_json::Value {
        serde_json::json!({
            "preferred": self.preferred(),
            "catalog_count": Self::catalog().map(|c| c.len()).unwrap_or(0),
            "documentation": "https://github.com/intellibitz/susi",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranked_frontier_catalog() {
        let catalog: Vec<FrontierDefinition> =
            serde_json::from_str(include_str!("../../../config/frontier-models.json")).unwrap();
        assert_eq!(catalog.len(), 5);
        for (i, m) in catalog.iter().enumerate() {
            assert_eq!(m.rank as usize, i + 1);
            m.validate().unwrap();
        }
        assert_eq!(catalog[0].id, "claude-sonnet");
        assert_eq!(catalog[1].id, "gpt");
        assert_eq!(catalog[2].model, "deepseek-reasoner");
        assert_eq!(catalog[3].family, "Google");
        assert_eq!(catalog[4].engine, "Ollama");
    }

    #[test]
    fn variant_lookup_and_override() {
        let root = std::env::temp_dir().join(format!(
            "susi-frontier-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let manager = FrontierManager::with_config(root.clone());
        // Bundled catalog via load_json_or_bundled — definition works.
        let gpt4o = manager.effective("gpt-4o").unwrap();
        assert_eq!(gpt4o.id, "gpt");
        assert_eq!(gpt4o.model, "gpt-4o");
        let updated = manager
            .configure(
                "gpt",
                &FrontierOverride {
                    engine: None,
                    model: Some("gpt-4o-mini".into()),
                    protocol_type: None,
                    api_key_env: None,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(updated.model, "gpt-4o-mini");
        assert_eq!(manager.reset("gpt").unwrap().model, "gpt-5");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn frontier_definition_lookup_rejects_unknown_and_validate_rejects_invalid_env() {
        assert!(FrontierManager::definition("unknown-frontier-xyz").is_err());
        let root = susi_paths::tempfile::tempdir().unwrap();
        let manager = FrontierManager::with_config(root.path().to_path_buf());
        let res = manager.configure(
            "gpt",
            &FrontierOverride {
                api_key_env: Some("BAD-ENV!".into()),
                ..Default::default()
            },
        );
        assert!(res.is_err());
    }

    #[test]
    fn clear_preferred_is_idempotent_and_status_reports_catalog_count() {
        let root = susi_paths::tempfile::tempdir().unwrap();
        let manager = FrontierManager::with_config(root.path().to_path_buf());
        assert_eq!(
            manager.clear_preferred().unwrap(),
            "no preferred frontier model was set"
        );
        let status = manager.status();
        assert_eq!(status["catalog_count"].as_u64().unwrap(), 5);
        assert!(status["preferred"].is_null());
    }

    #[test]
    fn setup_returns_structured_instructions_and_model_metadata() {
        let root = susi_paths::tempfile::tempdir().unwrap();
        let manager = FrontierManager::with_config(root.path().to_path_buf());
        let info = manager.setup("claude-sonnet").unwrap();
        assert_eq!(info["model"]["id"], "claude-sonnet");
        assert!(info["instructions"]
            .as_str()
            .unwrap_or_default()
            .contains("susi frontier doctor claude-sonnet"));
    }
}
