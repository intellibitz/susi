//! End-to-end open-weight frontier model control plane (local host via Ollama/vLLM).
//!
//! Catalog ranks are editorial (agentic reasoning / coding / local deploy).
//! Live probe / HTTP live against Ollama live in `susi_gemi::open_weight_ext`.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::cloud::{
    apply_cloud_env_file, effective_inference_endpoints_pub, is_remote_cloud, resolve_api_key,
};

pub const ENGINE_NAME: &str = "Ollama";
pub const DEFAULT_API_BASE: &str = "http://localhost:11434/v1";
pub const DOCUMENTATION: &str = "https://ollama.com/library";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenWeightVariant {
    pub id: String,
    pub ollama_tag: String,
    #[serde(default)]
    pub hf_repo: String,
    #[serde(default)]
    pub openrouter_model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenWeightDefinition {
    pub id: String,
    pub name: String,
    pub rank: u8,
    pub documentation: String,
    #[serde(default)]
    pub family: String,
    /// Preferred local OpenAI-compat engine (usually Ollama; vLLM also works).
    pub engine: String,
    pub ollama_tag: String,
    #[serde(default)]
    pub hf_repo: String,
    #[serde(default)]
    pub openrouter_model: String,
    #[serde(default)]
    pub protocol_type: String,
    #[serde(default)]
    pub variants: Vec<OpenWeightVariant>,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenWeightOverride {
    pub engine: Option<String>,
    pub ollama_tag: Option<String>,
    pub protocol_type: Option<String>,
}

#[derive(Clone)]
pub struct OpenWeightManager {
    root: PathBuf,
}

impl OpenWeightManager {
    pub fn new() -> Result<Self> {
        Ok(Self {
            root: susi_paths::SusiDirs::config_dir().join("open-weight"),
        })
    }

    pub fn with_root(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn catalog() -> Result<Vec<OpenWeightDefinition>> {
        Ok(susi_sandbox::extensions::load_json_or_bundled(
            "open-weight-models.json",
            include_str!("../../../config/open-weight-models.json"),
        ))
    }

    pub fn definition(id: &str) -> Result<OpenWeightDefinition> {
        let needle = id.trim();
        Self::catalog()?
            .into_iter()
            .find(|m| {
                m.id.eq_ignore_ascii_case(needle)
                    || m.name.eq_ignore_ascii_case(needle)
                    || m.ollama_tag.eq_ignore_ascii_case(needle)
                    || m.variants.iter().any(|v| {
                        v.id.eq_ignore_ascii_case(needle)
                            || v.ollama_tag.eq_ignore_ascii_case(needle)
                    })
            })
            .with_context(|| format!("unknown open-weight model {id}; use `susi openweight list`"))
    }

    pub fn effective(&self, id: &str) -> Result<OpenWeightDefinition> {
        let mut def = Self::definition(id)?;
        // If the lookup matched a variant id/tag, pin that variant as the effective tag.
        if let Some(variant) = def.variants.iter().find(|v| {
            v.id.eq_ignore_ascii_case(id.trim()) || v.ollama_tag.eq_ignore_ascii_case(id.trim())
        }) {
            def.ollama_tag = variant.ollama_tag.clone();
            if !variant.hf_repo.is_empty() {
                def.hf_repo = variant.hf_repo.clone();
            }
            if !variant.openrouter_model.is_empty() {
                def.openrouter_model = variant.openrouter_model.clone();
            }
        }
        let path = self.root.join(format!("{}.json", def.id));
        if let Ok(bytes) = fs::read(&path) {
            let over: OpenWeightOverride =
                serde_json::from_slice(&bytes).context("invalid open-weight override")?;
            if let Some(engine) = over.engine.filter(|s| !s.trim().is_empty()) {
                def.engine = engine;
            }
            if let Some(tag) = over.ollama_tag.filter(|s| !s.trim().is_empty()) {
                def.ollama_tag = tag;
            }
            if let Some(protocol) = over.protocol_type {
                def.protocol_type = protocol;
            }
        }
        def.validate()?;
        Ok(def)
    }

    pub fn configure(&self, id: &str, over: &OpenWeightOverride) -> Result<OpenWeightDefinition> {
        let def = Self::definition(id)?;
        private_dir(&self.root)?;
        atomic_json(&self.root.join(format!("{}.json", def.id)), over)?;
        self.effective(&def.id)
    }

    pub fn reset(&self, id: &str) -> Result<OpenWeightDefinition> {
        let def = Self::definition(id)?;
        match fs::remove_file(self.root.join(format!("{}.json", def.id))) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        self.effective(&def.id)
    }

    fn preferred_path(&self) -> PathBuf {
        self.root.join("preferred_model.txt")
    }

    pub fn preferred(&self) -> Option<String> {
        fs::read_to_string(self.preferred_path())
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    }

    pub fn endpoint_for(engine: &str) -> Option<susi_sandbox::manager::InferenceEndpointItem> {
        let lower = engine.to_ascii_lowercase();
        effective_inference_endpoints_pub()
            .into_iter()
            .find(|e| e.name.to_ascii_lowercase() == lower)
    }

    pub fn ollama_cli_present() -> bool {
        which("ollama").is_some()
    }

    /// Local readiness: endpoint + (for remote engines) credentials. Does not pull weights.
    pub fn doctor(&self, id: Option<&str>) -> Result<String> {
        apply_cloud_env_file();
        let ids: Vec<String> = match id {
            Some(one) => vec![Self::definition(one)?.id],
            None => Self::catalog()?.into_iter().map(|m| m.id).collect(),
        };
        let mut reports = Vec::new();
        for mid in ids {
            let def = self.effective(&mid)?;
            let endpoint = Self::endpoint_for(&def.engine).with_context(|| {
                format!(
                    "inference endpoint '{}' is not configured (need Ollama/vLLM api_base)",
                    def.engine
                )
            })?;
            if endpoint.api_base.trim().is_empty() {
                bail!("endpoint '{}' has an empty api_base", def.engine);
            }
            if is_remote_cloud(endpoint.api_base.trim()) {
                let key_env = endpoint.api_key_env.as_str();
                if !key_env.is_empty() && resolve_api_key(key_env, &def.engine).is_empty() {
                    bail!("set {key_env} for remote engine {}", def.engine);
                }
            }
            let pulled = self.model_pulled(&def.ollama_tag).unwrap_or(false);
            reports.push(format!(
                "{}: endpoint {} ready; tag {}; pulled={}; ollama_cli={}",
                def.id,
                def.engine,
                def.ollama_tag,
                pulled,
                Self::ollama_cli_present()
            ));
        }
        Ok(reports.join("\n"))
    }

    pub fn setup(&self, id: &str) -> Result<serde_json::Value> {
        let def = self.effective(id)?;
        let endpoint = Self::endpoint_for(&def.engine);
        let api_base = endpoint
            .as_ref()
            .map(|e| e.api_base.clone())
            .unwrap_or_else(|| DEFAULT_API_BASE.to_string());
        Ok(serde_json::json!({
            "model": def,
            "preferred": self.preferred(),
            "api_base": api_base,
            "ollama_cli_present": Self::ollama_cli_present(),
            "endpoint_configured": endpoint.is_some(),
            "instructions": format!(
                "1) Install Ollama (https://ollama.com) or point engine at vLLM/llama.cpp\n\
                 2) `susi openweight pull {id}` (or `ollama pull {}`)\n\
                 3) `susi openweight doctor {id}` then optional `susi openweight probe {id}`\n\
                 4) `susi openweight prefer {id}` to pin local routing\n\
                 Weights are FOSS-adjacent open-weight; no cloud subscriptions are provisioned.",
                def.ollama_tag
            )
        }))
    }

    pub fn status(&self) -> serde_json::Value {
        apply_cloud_env_file();
        let endpoint = Self::endpoint_for(ENGINE_NAME);
        serde_json::json!({
            "engine": ENGINE_NAME,
            "api_base": endpoint.as_ref().map(|e| e.api_base.clone()).unwrap_or_else(|| DEFAULT_API_BASE.to_string()),
            "endpoint_configured": endpoint.is_some(),
            "ollama_cli_present": Self::ollama_cli_present(),
            "preferred": self.preferred(),
            "documentation": DOCUMENTATION,
            "catalog_count": Self::catalog().map(|c| c.len()).unwrap_or(0),
        })
    }

    /// Pull weights via `ollama pull <tag>` when the CLI is available.
    pub fn pull(&self, id: &str) -> Result<String> {
        let def = self.effective(id)?;
        let ollama = which("ollama").context(
            "ollama CLI not found on PATH; install from https://ollama.com or host via vLLM",
        )?;
        validate_ollama_tag(&def.ollama_tag)?;
        let status = Command::new(&ollama)
            .args(["pull", &def.ollama_tag])
            .status()
            .with_context(|| format!("failed to spawn ollama pull {}", def.ollama_tag))?;
        if !status.success() {
            bail!(
                "ollama pull {} failed with status {}",
                def.ollama_tag,
                status
            );
        }
        Ok(format!(
            "pulled {}; prefer with `susi openweight prefer {}`",
            def.ollama_tag, def.id
        ))
    }

    /// Prefer this open-weight model for subsequent local routing.
    pub fn prefer(&self, id: &str) -> Result<String> {
        let def = self.effective(id)?;
        // Soft doctor: endpoint must exist; pull is recommended but not required to pin.
        let _ = Self::endpoint_for(&def.engine).with_context(|| {
            format!(
                "inference endpoint '{}' missing; configure Ollama/vLLM api_base first",
                def.engine
            )
        })?;
        private_dir(&self.root)?;
        fs::write(self.preferred_path(), &def.id)?;
        crate::ModelManager::set_selected_model(&def.ollama_tag).map_err(|e| anyhow::anyhow!(e))?;
        Ok(format!(
            "preferred open-weight model set to {} (ollama tag {})",
            def.id, def.ollama_tag
        ))
    }

    pub fn clear_preferred(&self) -> Result<String> {
        match fs::remove_file(self.preferred_path()) {
            Ok(()) => Ok("cleared preferred open-weight model".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok("no preferred open-weight model was set".into())
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Best-effort: ask Ollama `/api/tags` whether the tag is present.
    pub fn model_pulled(&self, tag: &str) -> Result<bool> {
        let endpoint = Self::endpoint_for(ENGINE_NAME)
            .or_else(|| Self::endpoint_for("vLLM"))
            .context("no local OpenAI-compat endpoint configured")?;
        let base = endpoint.api_base.trim().trim_end_matches('/').to_string();
        // Ollama native tags API is sibling to /v1.
        let tags_url = if let Some(stripped) = base.strip_suffix("/v1") {
            format!("{stripped}/api/tags")
        } else {
            format!("{base}/api/tags")
        };
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .context("http client for ollama tags")?;
        let res = client.get(&tags_url).send();
        let Ok(res) = res else {
            return Ok(false);
        };
        if !res.status().is_success() {
            return Ok(false);
        }
        let json: serde_json::Value = res.json().unwrap_or_default();
        let needle = tag.split(':').next().unwrap_or(tag).to_ascii_lowercase();
        let Some(models) = json.get("models").and_then(|m| m.as_array()) else {
            return Ok(false);
        };
        Ok(models.iter().any(|m| {
            m.get("name")
                .and_then(|n| n.as_str())
                .map(|n| {
                    let n = n.to_ascii_lowercase();
                    n == tag.to_ascii_lowercase()
                        || n.starts_with(&format!("{needle}:"))
                        || n == needle
                })
                .unwrap_or(false)
        }))
    }

    /// Resolve api_base + model tag for engines to build an HttpProvider.
    pub fn resolve_endpoint(&self, id: &str) -> Result<(OpenWeightDefinition, String, String)> {
        apply_cloud_env_file();
        let def = self.effective(id)?;
        let endpoint = Self::endpoint_for(&def.engine)
            .with_context(|| format!("inference endpoint '{}' missing", def.engine))?;
        let protocol = if def.protocol_type.is_empty() {
            endpoint.protocol_type.clone()
        } else {
            def.protocol_type.clone()
        };
        Ok((def, endpoint.api_base.trim().to_string(), protocol))
    }
}

impl OpenWeightDefinition {
    fn validate(&self) -> Result<()> {
        if self.id.trim().is_empty()
            || self.engine.trim().is_empty()
            || self.ollama_tag.trim().is_empty()
        {
            bail!("open-weight id, engine, and ollama_tag must be nonempty");
        }
        validate_ollama_tag(&self.ollama_tag)?;
        Ok(())
    }
}

fn validate_ollama_tag(tag: &str) -> Result<()> {
    if tag.is_empty() || tag.contains("..") || tag.contains(' ') || tag.contains('/') {
        bail!("ollama_tag must be an Ollama library tag (e.g. deepseek-r1), not a path");
    }
    if tag.len() > 120 {
        bail!("ollama tag too long");
    }
    if !tag
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b':')
    {
        bail!("ollama tag has invalid characters");
    }
    Ok(())
}

fn which(bin: &str) -> Option<PathBuf> {
    if let Ok(p) = std::env::var(format!("SUSI_{}_BIN", bin.to_ascii_uppercase())) {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Some(path);
        }
    }
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(bin);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let exe = dir.join(format!("{bin}.exe"));
            if exe.is_file() {
                return Some(exe);
            }
        }
    }
    None
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

fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    use std::fs::OpenOptions;
    use std::io::Write;
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
    fn ranked_open_weight_catalog() {
        let catalog = OpenWeightManager::catalog().unwrap();
        assert_eq!(catalog.len(), 5);
        for (i, m) in catalog.iter().enumerate() {
            assert_eq!(m.rank as usize, i + 1);
            m.validate().unwrap();
            assert!(!m.ollama_tag.is_empty());
            assert!(!m.hf_repo.is_empty());
        }
        assert_eq!(
            OpenWeightManager::definition("DeepSeek-R1 / V3")
                .unwrap()
                .id,
            "deepseek-r1"
        );
        assert_eq!(
            OpenWeightManager::definition("phi4").unwrap().ollama_tag,
            "phi4"
        );
        assert_eq!(
            OpenWeightManager::definition("llama-3.2")
                .unwrap()
                .variants
                .iter()
                .any(|v| v.id == "llama-3.2"),
            true
        );
    }

    #[test]
    fn validate_ollama_tag_rejects_paths() {
        assert!(validate_ollama_tag("deepseek-r1").is_ok());
        assert!(validate_ollama_tag("qwen2.5-coder:7b").is_ok());
        assert!(validate_ollama_tag("../evil").is_err());
        assert!(validate_ollama_tag("org/model").is_err());
    }

    #[test]
    fn host_override_and_reset() {
        let root = std::env::temp_dir().join(format!(
            "susi-open-weight-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let manager = OpenWeightManager::with_root(root.clone());
        let updated = manager
            .configure(
                "phi-4",
                &OpenWeightOverride {
                    engine: None,
                    ollama_tag: Some("phi4:14b".into()),
                    protocol_type: None,
                },
            )
            .unwrap();
        assert_eq!(updated.ollama_tag, "phi4:14b");
        assert_eq!(manager.effective("phi-4").unwrap().ollama_tag, "phi4:14b");
        assert_eq!(manager.reset("phi-4").unwrap().ollama_tag, "phi4");
        let _ = fs::remove_dir_all(root);
    }
}
