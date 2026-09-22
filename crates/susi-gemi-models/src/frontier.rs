//! End-to-end top frontier model control plane (planning / tool calling / long-horizon).
//!
//! Catalog ranks are editorial industry consensus. Paid/local probe lives in
//! `susi_gemi::frontier_ext` so this crate never depends on engines HTTP.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::cloud::{
    apply_cloud_env_file, effective_inference_endpoints_pub, is_remote_cloud, resolve_api_key,
};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrontierOverride {
    pub engine: Option<String>,
    pub model: Option<String>,
    pub protocol_type: Option<String>,
    pub api_key_env: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FrontierEndpoint {
    pub def: FrontierDefinition,
    pub api_base: String,
    pub protocol_type: String,
    pub api_key_env: String,
    pub api_key: String,
}

#[derive(Clone)]
pub struct FrontierManager {
    config: PathBuf,
}

impl FrontierManager {
    pub fn new() -> Result<Self> {
        Ok(Self {
            config: susi_paths::SusiDirs::config_dir().join("frontier-models"),
        })
    }

    pub fn with_config(config: PathBuf) -> Self {
        Self { config }
    }

    pub fn catalog() -> Result<Vec<FrontierDefinition>> {
        Ok(susi_sandbox::extensions::load_json_or_bundled(
            "frontier-models.json",
            include_str!("../../../config/frontier-models.json"),
        ))
    }

    pub fn definition(id: &str) -> Result<FrontierDefinition> {
        let needle = id.trim();
        Self::catalog()?
            .into_iter()
            .find(|m| {
                m.id.eq_ignore_ascii_case(needle)
                    || m.name.eq_ignore_ascii_case(needle)
                    || m.model.eq_ignore_ascii_case(needle)
                    || m.variants.iter().any(|v| {
                        v.id.eq_ignore_ascii_case(needle) || v.model.eq_ignore_ascii_case(needle)
                    })
            })
            .with_context(|| format!("unknown frontier model {id}; use `susi frontier list`"))
    }

    pub fn effective(&self, id: &str) -> Result<FrontierDefinition> {
        let mut def = Self::definition(id)?;
        if let Some(variant) = def.variants.iter().find(|v| {
            v.id.eq_ignore_ascii_case(id.trim()) || v.model.eq_ignore_ascii_case(id.trim())
        }) {
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
        let path = self.config.join(format!("{}.json", def.id));
        if let Ok(bytes) = fs::read(&path) {
            let over: FrontierOverride =
                serde_json::from_slice(&bytes).context("invalid frontier-model override")?;
            if let Some(engine) = over.engine.filter(|s| !s.trim().is_empty()) {
                def.engine = engine;
            }
            if let Some(model) = over.model.filter(|s| !s.trim().is_empty()) {
                def.model = model;
            }
            if let Some(protocol) = over.protocol_type {
                def.protocol_type = protocol;
            }
            if let Some(env) = over.api_key_env {
                def.api_key_env = env;
            }
        }
        def.validate()?;
        Ok(def)
    }

    pub fn configure(&self, id: &str, over: &FrontierOverride) -> Result<FrontierDefinition> {
        let def = Self::definition(id)?;
        private_dir(&self.config)?;
        atomic_json(&self.config.join(format!("{}.json", def.id)), over)?;
        self.effective(&def.id)
    }

    pub fn reset(&self, id: &str) -> Result<FrontierDefinition> {
        let def = Self::definition(id)?;
        match fs::remove_file(self.config.join(format!("{}.json", def.id))) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        self.effective(&def.id)
    }

    fn preferred_path(&self) -> PathBuf {
        self.config.join("preferred_model.txt")
    }

    pub fn preferred(&self) -> Option<String> {
        fs::read_to_string(self.preferred_path())
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    }

    /// Local readiness only — does not call a paid model.
    pub fn preflight(&self, id: &str) -> Result<String> {
        apply_cloud_env_file();
        let def = self.effective(id)?;
        let endpoint = endpoint_for(&def.engine)
            .with_context(|| format!("inference endpoint '{}' is not configured", def.engine))?;
        if endpoint.api_base.trim().is_empty() {
            bail!("endpoint '{}' has an empty api_base", def.engine);
        }
        let key_env = if def.api_key_env.is_empty() {
            endpoint.api_key_env.as_str()
        } else {
            def.api_key_env.as_str()
        };
        let key = resolve_api_key(key_env, &def.engine);
        if is_remote_cloud(endpoint.api_base.trim()) && !key_env.is_empty() && key.is_empty() {
            bail!(
                "set {key_env} (or `susi keys set {}`) for cloud access",
                def.engine
            );
        }
        Ok(format!(
            "endpoint {} ready; model id {}; credentials {}",
            def.engine,
            def.model,
            if key_env.is_empty() || !is_remote_cloud(endpoint.api_base.trim()) {
                "not required (local)"
            } else {
                "present"
            }
        ))
    }

    pub fn resolve_endpoint(&self, id: &str) -> Result<FrontierEndpoint> {
        apply_cloud_env_file();
        let def = self.effective(id)?;
        self.preflight(&def.id)?;
        let endpoint = endpoint_for(&def.engine).context("endpoint missing after preflight")?;
        let key_env = if def.api_key_env.is_empty() {
            endpoint.api_key_env.clone()
        } else {
            def.api_key_env.clone()
        };
        let protocol_type = if def.protocol_type.is_empty() {
            endpoint.protocol_type.clone()
        } else {
            def.protocol_type.clone()
        };
        let api_key = resolve_api_key(&key_env, &def.engine);
        Ok(FrontierEndpoint {
            def,
            api_base: endpoint.api_base.trim().to_string(),
            protocol_type,
            api_key_env: key_env,
            api_key,
        })
    }

    pub fn prefer(&self, id: &str) -> Result<String> {
        let def = self.effective(id)?;
        self.preflight(id)?;
        private_dir(&self.config)?;
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
                },
            )?;
        }
        fs::write(self.preferred_path(), &def.id)?;
        crate::ModelManager::set_selected_model(&def.model).map_err(|e| anyhow::anyhow!(e))?;
        Ok(format!(
            "preferred frontier model set to {} (api id {})",
            def.id, def.model
        ))
    }

    pub fn clear_preferred(&self) -> Result<String> {
        match fs::remove_file(self.preferred_path()) {
            Ok(()) => Ok("cleared preferred frontier model".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok("no preferred frontier model was set".into())
            }
            Err(e) => Err(e.into()),
        }
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
        apply_cloud_env_file();
        serde_json::json!({
            "preferred": self.preferred(),
            "catalog_count": Self::catalog().map(|c| c.len()).unwrap_or(0),
            "documentation": "https://github.com/intellibitz/susi",
        })
    }
}

impl FrontierDefinition {
    fn validate(&self) -> Result<()> {
        if self.id.trim().is_empty()
            || self.engine.trim().is_empty()
            || self.model.trim().is_empty()
        {
            bail!("frontier model id, engine, and model must be nonempty");
        }
        if !self.api_key_env.is_empty()
            && !self
                .api_key_env
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            bail!("api_key_env must name an environment variable");
        }
        Ok(())
    }
}

fn endpoint_for(name: &str) -> Option<susi_sandbox::manager::InferenceEndpointItem> {
    let lower = name.to_ascii_lowercase();
    effective_inference_endpoints_pub()
        .into_iter()
        .find(|e| e.name.to_ascii_lowercase() == lower)
}

fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let tmp = path.with_extension("tmp");
    let result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        serde_json::to_writer_pretty(&mut file, value)?;
        file.flush()?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
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
                },
            )
            .unwrap();
        assert_eq!(updated.model, "gpt-4o-mini");
        assert_eq!(manager.reset("gpt").unwrap().model, "gpt-5");
        let _ = fs::remove_dir_all(root);
    }
}
