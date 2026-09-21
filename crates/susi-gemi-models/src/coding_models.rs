//! Durable management of the top developer/agent-focused cloud models.
//! Catalog ranks are editorial (coding + agents), not chatbot popularity.
//!
//! Paid probe / `HttpProvider` construction lives in the engines crate
//! (`susi_gemi::coding_models_ext`) so this models crate never depends on engines.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::cloud::{
    apply_cloud_env_file, effective_inference_endpoints_pub, is_remote_cloud, resolve_api_key,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodingModelDefinition {
    pub id: String,
    pub name: String,
    pub rank: u8,
    pub documentation: String,
    pub engine: String,
    /// Provider API model id (overridable via host configure).
    pub model: String,
    #[serde(default)]
    pub protocol_type: String,
    #[serde(default)]
    pub api_key_env: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodingModelOverride {
    pub engine: Option<String>,
    pub model: Option<String>,
    pub protocol_type: Option<String>,
    pub api_key_env: Option<String>,
}

/// Resolved endpoint + credential metadata for engines to build an `HttpProvider`.
#[derive(Debug, Clone)]
pub struct CodingModelEndpoint {
    pub def: CodingModelDefinition,
    pub api_base: String,
    pub protocol_type: String,
    pub api_key_env: String,
    pub api_key: String,
}

#[derive(Clone)]
pub struct CodingModelManager {
    config: PathBuf,
}

impl CodingModelManager {
    pub fn new() -> Result<Self> {
        let config = susi_paths::SusiDirs::config_dir().join("coding-models");
        Ok(Self { config })
    }

    pub fn with_config(config: PathBuf) -> Self {
        Self { config }
    }

    pub fn catalog() -> Result<Vec<CodingModelDefinition>> {
        // Extension-pack API: host `~/.susi/extensions/<pack>/coding-models.json`
        // overrides the bundled catalog (manifest may still point at config/).
        Ok(susi_sandbox::extensions::load_json_or_bundled(
            "coding-models.json",
            include_str!("../../../config/coding-models.json"),
        ))
    }

    pub fn definition(id: &str) -> Result<CodingModelDefinition> {
        Self::catalog()?
            .into_iter()
            .find(|m| m.id == id || m.name.eq_ignore_ascii_case(id))
            .with_context(|| format!("unknown coding model {id}; use `susi models list`"))
    }

    pub fn effective(&self, id: &str) -> Result<CodingModelDefinition> {
        let mut def = Self::definition(id)?;
        let path = self.config.join(format!("{}.json", def.id));
        if let Ok(bytes) = fs::read(&path) {
            let over: CodingModelOverride =
                serde_json::from_slice(&bytes).context("invalid coding-model override")?;
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

    pub fn configure(&self, id: &str, over: &CodingModelOverride) -> Result<CodingModelDefinition> {
        let def = Self::definition(id)?;
        private_dir(&self.config)?;
        atomic_json(&self.config.join(format!("{}.json", def.id)), over)?;
        self.effective(&def.id)
    }

    pub fn reset(&self, id: &str) -> Result<CodingModelDefinition> {
        let def = Self::definition(id)?;
        match fs::remove_file(self.config.join(format!("{}.json", def.id))) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        self.effective(&def.id)
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
        if is_remote_cloud(endpoint.api_base.trim()) && key.is_empty() {
            bail!(
                "set {key_env} (or `susi keys set {}`) for cloud access",
                def.engine
            );
        }
        Ok(format!(
            "endpoint {} ready; model id {}; credentials present",
            def.engine, def.model
        ))
    }

    /// Resolve endpoint + key metadata for engines to construct an HTTP provider.
    pub fn resolve_endpoint(&self, id: &str) -> Result<CodingModelEndpoint> {
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
        Ok(CodingModelEndpoint {
            def,
            api_base: endpoint.api_base.trim().to_string(),
            protocol_type,
            api_key_env: key_env,
            api_key,
        })
    }

    pub fn prefer(&self, id: &str) -> Result<String> {
        let def = self.effective(id)?;
        self.preflight(&def.id)?;
        private_dir(&susi_paths::SusiDirs::config_dir())?;
        let prefer = susi_paths::SusiDirs::config_dir().join("preferred_coding_model.txt");
        fs::write(&prefer, &def.id)?;
        // Also set the runtime override to the provider model id for cloud routing.
        crate::ModelManager::set_selected_model(&def.model).map_err(|e| anyhow::anyhow!(e))?;
        Ok(format!(
            "preferred coding model set to {} (api id {})",
            def.id, def.model
        ))
    }

    pub fn preferred(&self) -> Option<String> {
        fs::read_to_string(susi_paths::SusiDirs::config_dir().join("preferred_coding_model.txt"))
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    }

    /// Zero-config: if no preferred coding model is set, prefer the best-ranked
    /// catalog entry whose endpoint + credentials are ready.
    pub fn auto_prefer_best_ready(&self) -> Result<Option<String>> {
        if let Some(existing) = self.preferred() {
            // Refresh selection if still ready; otherwise fall through to pick another.
            if self.preflight(&existing).is_ok() {
                return Ok(Some(existing));
            }
        }
        let mut catalog = Self::catalog()?;
        catalog.sort_by_key(|m| m.rank);
        for def in catalog {
            if self.preflight(&def.id).is_err() {
                continue;
            }
            self.prefer(&def.id)?;
            if std::env::var("SUSI_VERBOSE").is_ok() {
                eprintln!("[AUTO] Preferred coding model `{}`", def.id);
            }
            return Ok(Some(def.id));
        }
        Ok(None)
    }
}

impl CodingModelDefinition {
    fn validate(&self) -> Result<()> {
        if self.id.trim().is_empty()
            || self.engine.trim().is_empty()
            || self.model.trim().is_empty()
        {
            bail!("coding model id, engine, and model must be nonempty");
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
    fn ten_ranked_coding_models() {
        let catalog = CodingModelManager::catalog().unwrap();
        assert_eq!(catalog.len(), 10);
        for (i, m) in catalog.iter().enumerate() {
            assert_eq!(m.rank as usize, i + 1);
            m.validate().unwrap();
            assert!(!m.model.is_empty());
            assert!(!m.engine.is_empty());
        }
        assert_eq!(
            CodingModelManager::definition("Claude Opus").unwrap().id,
            "claude-opus"
        );
        assert_eq!(
            CodingModelManager::definition("llama-4").unwrap().engine,
            "OpenRouter"
        );
    }

    #[test]
    fn host_override_and_reset() {
        let root = std::env::temp_dir().join(format!(
            "susi-coding-model-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let manager = CodingModelManager::with_config(root.clone());
        let updated = manager
            .configure(
                "gpt-5",
                &CodingModelOverride {
                    engine: None,
                    model: Some("gpt-5.4".into()),
                    protocol_type: None,
                    api_key_env: None,
                },
            )
            .unwrap();
        assert_eq!(updated.model, "gpt-5.4");
        assert_eq!(manager.effective("gpt-5").unwrap().model, "gpt-5.4");
        assert_eq!(manager.reset("gpt-5").unwrap().model, "gpt-5");
        let _ = fs::remove_dir_all(root);
    }
}
