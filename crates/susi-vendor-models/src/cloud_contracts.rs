//! Fresh model-level capability facts from supported model catalog metadata.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use susi_error::{EaiError, EaiResult};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Contract {
    pub model: String,
    pub context_tokens: Option<u64>,
    #[serde(default)]
    pub price: Option<crate::cloud_budget::Price>,
    pub input_modalities: Vec<String>,
    pub supported_parameters: Vec<String>,
    pub observed_at: u64,
    pub expires_at: u64,
}

impl Contract {
    pub fn supports(&self, requirement: &str, now: u64) -> bool {
        if now < self.observed_at || now >= self.expires_at {
            return false;
        }
        match requirement.to_ascii_lowercase().as_str() {
            "vision" => self.input_modalities.iter().any(|s| s == "image"),
            "tools" | "tool-calling" => self.supported_parameters.iter().any(|s| s == "tools"),
            "structured-output" => self
                .supported_parameters
                .iter()
                .any(|s| s == "structured_outputs"),
            "audio" => self.input_modalities.iter().any(|s| s == "audio"),
            "text" | "chat" | "reasoning" | "code" => {
                self.input_modalities.iter().any(|s| s == "text")
            }
            _ => false,
        }
    }
}

pub fn directory() -> PathBuf {
    susi_paths::SusiDirs::data_dir().join("cloud-contracts")
}

fn path(dir: &Path, endpoint: &str) -> PathBuf {
    use sha2::{Digest, Sha256};
    dir.join(format!(
        "{}.json",
        hex::encode(Sha256::digest(endpoint.as_bytes()))
    ))
}

pub fn load(dir: &Path, endpoint: &str) -> EaiResult<Vec<Contract>> {
    let file = match std::fs::File::open(path(dir, endpoint)) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(EaiError::io(e.to_string())),
    };
    use std::io::Read;
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| EaiError::io(e.to_string()))?;
    if bytes.len() > 1024 * 1024 {
        return Err(EaiError::config("cloud contracts exceed limit"));
    }
    let records: Vec<Contract> =
        serde_json::from_slice(&bytes).map_err(|_| EaiError::config("invalid cloud contracts"))?;
    if records.len() > 1024 {
        return Err(EaiError::config("too many cloud contracts"));
    }
    Ok(records)
}

/// OpenRouter-style model metadata. Absent fields stay unknown, and model
/// ids alone never imply capabilities. No network request is initiated here.
pub fn observe_catalog(
    dir: &Path,
    endpoint: &str,
    value: &serde_json::Value,
    now: u64,
) -> EaiResult<()> {
    let Some(models) = value["data"].as_array() else {
        return Ok(());
    };
    let strings = |v: &serde_json::Value| {
        v.as_array()
            .map(|rows| {
                rows.iter()
                    .take(32)
                    .filter_map(|v| v.as_str().filter(|s| s.len() <= 64).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    let contracts: Vec<_> = models
        .iter()
        .take(1024)
        .filter_map(|v| {
            let model = v["id"].as_str()?.to_string();
            if model.len() > 256 || v.get("architecture").is_none() {
                return None;
            }
            Some(Contract {
                model,
                context_tokens: v["context_length"].as_u64(),
                price: crate::cloud_budget::Price::from_catalog(&v["pricing"]),
                input_modalities: strings(&v["architecture"]["input_modalities"]),
                supported_parameters: strings(&v["supported_parameters"]),
                observed_at: now,
                expires_at: now.saturating_add(3600),
            })
        })
        .collect();
    if contracts.is_empty() {
        return Ok(());
    }
    let bytes = serde_json::to_vec(&contracts)
        .map_err(|_| EaiError::config("cannot encode cloud contracts"))?;
    if bytes.len() > 1024 * 1024 {
        return Err(EaiError::config("cloud contracts exceed limit"));
    }
    std::fs::create_dir_all(dir).map_err(|e| EaiError::io(e.to_string()))?;
    susi_config::atomic_write_bytes(&path(dir, endpoint), &bytes)
        .map_err(|e| EaiError::io(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn model_contracts_never_infer_capabilities_from_names() {
        let dir = tempfile::tempdir().unwrap();
        observe_catalog(
            dir.path(),
            "endpoint",
            &serde_json::json!({"data":[{"id":"vision-powerful"}]}),
            10,
        )
        .unwrap();
        assert!(load(dir.path(), "endpoint").unwrap().is_empty());
        observe_catalog(dir.path(),"endpoint",&serde_json::json!({"data":[{"id":"plain","context_length":4096,"architecture":{"input_modalities":["text","image"]},"supported_parameters":["tools"]}]}),10).unwrap();
        let rows = load(dir.path(), "endpoint").unwrap();
        assert!(rows[0].supports("vision", 11));
        assert!(rows[0].supports("tools", 11));
        assert!(!rows[0].supports("vision", 3610));
    }
}
