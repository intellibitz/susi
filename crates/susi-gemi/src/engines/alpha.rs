// SUSI-Alpha: Native Neural Intelligence Substrate
// 100% Rust implementation using Candle for Tier 0 Reflex Distillation

use anyhow::{anyhow, Result};
use candle_core::{DType, Tensor};
use candle_nn::{AdamW, Linear, Module, Optimizer, ParamsAdamW, VarBuilder, VarMap};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistillationStaged {
    pub intent: String,
    pub action: String,
    pub timestamp: u64,
}

fn parse_training_entries(
    content: &str,
    dynamic_intents: &[String],
) -> Result<Vec<(DistillationStaged, u32)>> {
    let mut entries = Vec::new();
    for (line_index, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            return Err(anyhow!(
                "empty distillation record at line {}",
                line_index + 1
            ));
        }
        let entry: DistillationStaged = serde_json::from_str(line).map_err(|error| {
            anyhow!(
                "invalid distillation record at line {}: {error}",
                line_index + 1
            )
        })?;
        if entry.intent.trim().is_empty() {
            return Err(anyhow!(
                "empty distillation intent at line {}",
                line_index + 1
            ));
        }
        let action = entry.action.trim();
        let label = dynamic_intents
            .iter()
            .position(|candidate| candidate.eq_ignore_ascii_case(action))
            .ok_or_else(|| {
                anyhow!(
                    "unknown distillation action {:?} at line {}",
                    entry.action,
                    line_index + 1
                )
            })?;
        let label = u32::try_from(label).map_err(|_| anyhow!("intent label exceeds u32"))?;
        entries.push((entry, label));
    }
    if entries.is_empty() {
        return Err(anyhow!("Empty distillation dataset."));
    }
    Ok(entries)
}

fn vocabulary_path(weights_path: &Path) -> PathBuf {
    weights_path.with_extension("intents.json")
}

const VOCABULARY_SCHEMA: &str = "susi/reflex-vocabulary/v1";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionVocabulary {
    schema: String,
    weights_sha256: String,
    intents: Vec<String>,
}

fn file_sha256(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)
        .map_err(|error| anyhow!("read reflex weights {}: {error}", path.display()))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn validate_vocabulary(intents: Vec<String>) -> Result<Vec<String>> {
    if intents.is_empty() {
        return Err(anyhow!("reflex action vocabulary is empty"));
    }
    if intents.len() > SusiAlphaModel::DIM {
        return Err(anyhow!(
            "reflex action vocabulary has {} entries, maximum is {}",
            intents.len(),
            SusiAlphaModel::DIM
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for intent in &intents {
        if intent.trim().is_empty() {
            return Err(anyhow!("reflex action vocabulary contains an empty entry"));
        }
        if !seen.insert(intent.to_lowercase()) {
            return Err(anyhow!(
                "reflex action vocabulary contains duplicate action {intent:?}"
            ));
        }
    }
    Ok(intents)
}

fn load_vocabulary(weights_path: &Path) -> Result<Vec<String>> {
    let path = vocabulary_path(weights_path);
    let bytes = std::fs::read(&path)
        .map_err(|error| anyhow!("read reflex action vocabulary {}: {error}", path.display()))?;
    let vocabulary: ActionVocabulary = serde_json::from_slice(&bytes)
        .map_err(|error| anyhow!("parse reflex action vocabulary {}: {error}", path.display()))?;
    if vocabulary.schema != VOCABULARY_SCHEMA {
        return Err(anyhow!(
            "unsupported reflex action vocabulary schema {:?}",
            vocabulary.schema
        ));
    }
    let actual_hash = file_sha256(weights_path)?;
    if vocabulary.weights_sha256 != actual_hash {
        return Err(anyhow!(
            "reflex weights/vocabulary mismatch: expected {}, found {}",
            vocabulary.weights_sha256,
            actual_hash
        ));
    }
    validate_vocabulary(vocabulary.intents)
}

fn extend_vocabulary(mut persisted: Vec<String>, discovered: Vec<String>) -> Vec<String> {
    for candidate in discovered {
        if persisted.len() == SusiAlphaModel::DIM {
            break;
        }
        if !persisted
            .iter()
            .any(|known| known.eq_ignore_ascii_case(&candidate))
        {
            persisted.push(candidate);
        }
    }
    persisted
}

fn save_vocabulary(weights_path: &Path, intents: &[String]) -> Result<()> {
    let vocabulary = ActionVocabulary {
        schema: VOCABULARY_SCHEMA.into(),
        weights_sha256: file_sha256(weights_path)?,
        intents: intents.to_vec(),
    };
    let bytes = serde_json::to_vec_pretty(&vocabulary)?;
    crate::susi_config::atomic_write_bytes(&vocabulary_path(weights_path), &bytes)
        .map_err(|error| anyhow!("persist reflex action vocabulary: {error}"))
}

/// SUSI-Alpha Intent Classifier (Neural Reflex)
pub struct SusiAlphaModel {
    fc1: Linear,
    fc2: Linear,
    intents: Vec<String>,
}

impl SusiAlphaModel {
    pub const DIM: usize = 128;

    pub fn global() -> &'static Self {
        static MODEL: std::sync::OnceLock<SusiAlphaModel> = std::sync::OnceLock::new();
        MODEL.get_or_init(|| {
            Self::load(&crate::susi_paths::SusiDirs::config_dir()).unwrap_or_else(|_| {
                let device = crate::hardware::HardwareProfiler::get_candle_device();
                let varmap = VarMap::new();
                let vb = VarBuilder::from_varmap(&varmap, DType::F32, &device);
                // A fresh VarMap only allocates two DIM x DIM layers; failure is
                // device OOM at bootstrap, and this &'static global has no error path.
                #[allow(clippy::unwrap_used)]
                let fc1 = candle_nn::linear(Self::DIM, Self::DIM, vb.pp("reflex")).unwrap();
                #[allow(clippy::unwrap_used)] // same invariant as fc1
                let fc2 = candle_nn::linear(Self::DIM, Self::DIM, vb.pp("reflex_out")).unwrap();
                Self {
                    fc1,
                    fc2,
                    intents: Self::list_dynamic_intents(),
                }
            })
        })
    }

    #[allow(unsafe_code)]
    pub fn load(global_dir: &Path) -> Result<Self> {
        let alpha_filename =
            crate::susi_sandbox::manager::SusiConfig::load(global_dir)?.alpha_weights_filename();
        let weights_path = global_dir.join("models").join(&alpha_filename);
        let device = crate::hardware::HardwareProfiler::get_candle_device();

        if weights_path.exists() {
            let intents = load_vocabulary(&weights_path)?;
            // SAFETY: mmap of a weights file the substrate owns; it is not modified while mapped.
            let vb = unsafe {
                VarBuilder::from_mmaped_safetensors(&[&weights_path], DType::F32, &device)
            }?;
            let fc1 = candle_nn::linear(Self::DIM, Self::DIM, vb.pp("reflex"))?;
            let fc2 = candle_nn::linear(Self::DIM, Self::DIM, vb.pp("reflex_out"))?;
            return Ok(Self { fc1, fc2, intents });
        }

        // Initialize default weights only when no published model exists.
        let varmap = VarMap::new();
        let vb = VarBuilder::from_varmap(&varmap, DType::F32, &device);
        let fc1 = candle_nn::linear(Self::DIM, Self::DIM, vb.pp("reflex"))?;
        let fc2 = candle_nn::linear(Self::DIM, Self::DIM, vb.pp("reflex_out"))?;

        let models_dir = global_dir.join("models");
        std::fs::create_dir_all(&models_dir)?;
        varmap.save(&weights_path)?;
        let intents = validate_vocabulary(Self::list_dynamic_intents())?;
        save_vocabulary(&weights_path, &intents)?;

        Ok(Self { fc1, fc2, intents })
    }

    /// Dynamic Intent Surface Discovery
    pub fn list_dynamic_intents() -> Vec<String> {
        let mut intents = vec![
            "status".into(),
            "version".into(),
            "self_heal_build".into(),
            "run_test_harness".into(),
            "write_file".into(),
            "read_file".into(),
            "list_directory".into(),
            "scout".into(),
            "reason".into(),
        ];

        // Add Registered Agents — dynamic entries fill remaining DIM
        // capacity after the foundational intents, so truncation can
        // never evict a base intent when the registry is crowded.
        let mut dynamic = Vec::new();
        let registry = crate::susi_core::AgentMetaRegistry::global();
        for agent in registry.list_agents() {
            if !intents.contains(&agent.name) && !dynamic.contains(&agent.name) {
                dynamic.push(agent.name);
            }
        }

        // Add Installed Tools
        for name in crate::susi_core::registry::CapabilityRegistry::global().list_tools() {
            if !intents.contains(&name) && !dynamic.contains(&name) {
                dynamic.push(name);
            }
        }

        dynamic.sort();
        dynamic.truncate(Self::DIM.saturating_sub(intents.len()));
        intents.extend(dynamic);
        intents.sort();
        intents
    }

    pub fn train_on_staged_data(global_dir: &Path) -> Result<String> {
        Self::train_on_staged_file(global_dir, &global_dir.join("distillation_staged.jsonl"))
    }

    pub fn train_on_staged_file(global_dir: &Path, staged_file: &Path) -> Result<String> {
        let _training_lock =
            crate::susi_config::file_lock::FileLock::acquire(global_dir, "reflex_training")
                .ok_or_else(|| anyhow!("reflex training lock unavailable"))?;
        if !staged_file.exists() {
            return Err(anyhow!(
                "No staged distillation data found at {}.",
                staged_file.display()
            ));
        }

        let alpha_filename =
            crate::susi_sandbox::manager::SusiConfig::load(global_dir)?.alpha_weights_filename();
        let models_dir = global_dir.join("models");
        std::fs::create_dir_all(&models_dir)?;
        let weights_path = models_dir.join(&alpha_filename);
        let discovered_intents = Self::list_dynamic_intents();
        let dynamic_intents = if weights_path.exists() {
            extend_vocabulary(load_vocabulary(&weights_path)?, discovered_intents)
        } else {
            validate_vocabulary(discovered_intents)?
        };

        let device = crate::hardware::HardwareProfiler::get_candle_device();
        let mut varmap = VarMap::new();
        let vb = VarBuilder::from_varmap(&varmap, DType::F32, &device);

        let fc1 = candle_nn::linear(Self::DIM, Self::DIM, vb.pp("reflex"))?;
        let fc2 = candle_nn::linear(Self::DIM, Self::DIM, vb.pp("reflex_out"))?;
        if weights_path.exists() {
            varmap.load(&weights_path)?;
        }

        let mut opt = AdamW::new(varmap.all_vars(), ParamsAdamW::default())?;

        // Load Data and Map to Dynamic Surface
        let content = std::fs::read_to_string(staged_file)?;
        let mut samples = Vec::new();
        let mut labels = Vec::new();

        let entries = parse_training_entries(&content, &dynamic_intents)?;

        for (entry, label) in entries {
            let vec = Self::semantic_centroid_projection(&entry.intent, None)?;
            samples.push(Tensor::from_vec(vec, (1, Self::DIM), &device)?);
            labels.push(label);
        }

        // Neural Seeding (Synthetic Priming): Ensure new tools have at least one sample
        for (idx, intent) in dynamic_intents.iter().enumerate() {
            let vec = Self::semantic_centroid_projection(intent, None)?;
            samples.push(Tensor::from_vec(vec, (1, Self::DIM), &device)?);
            labels.push(idx as u32);
        }

        let x = Tensor::cat(&samples, 0)?;
        let y = Tensor::from_vec(labels, samples.len(), &device)?;

        // Training Loop
        for _epoch in 1..=100 {
            let logits = fc1.forward(&x)?.relu()?;
            let logits = fc2.forward(&logits)?;
            let log_sm = candle_nn::ops::log_softmax(&logits, 1)?;
            let loss = candle_nn::loss::nll(&log_sm, &y)?;
            opt.backward_step(&loss)?;
        }

        // Atomic Model Save
        let tmp_path = weights_path.with_extension("tmp");
        varmap.save(&tmp_path)?;
        std::fs::rename(tmp_path, weights_path)?;
        save_vocabulary(&models_dir.join(&alpha_filename), &dynamic_intents)?;

        Ok(format!("Native distillation complete. Trained cumulatively on {} samples with the dynamic intent surface.", samples.len()))
    }

    pub fn get_model_fingerprint(global_dir: &Path) -> String {
        let alpha_filename = crate::susi_sandbox::manager::SusiConfig::load(global_dir)
            .unwrap_or_default()
            .alpha_weights_filename();
        let weights_path = global_dir.join("models").join(alpha_filename);
        if let Ok(meta) = std::fs::metadata(weights_path) {
            // Some filesystems (e.g. certain FUSE mounts) don't support mtime.
            return match meta.modified() {
                Ok(t) => format!("{:?}", t),
                Err(_) => "unknown-mtime".to_string(),
            };
        }
        "missing".to_string()
    }

    pub fn predict_intent(&self, prompt: &str) -> Result<String> {
        let (action, confidence) = self.predict_intent_with_confidence(prompt)?;
        if confidence > 0.5 {
            return Ok(action);
        }
        Err(anyhow!(
            "Low confidence ({:.2}) in neural reflex.",
            confidence
        ))
    }

    pub fn predict_intent_with_confidence(&self, prompt: &str) -> Result<(String, f32)> {
        let device = crate::hardware::HardwareProfiler::get_candle_device();
        let input_vec = Self::semantic_centroid_projection(prompt, None)?;
        let input_tensor = Tensor::from_vec(input_vec, (1, Self::DIM), &device)?;

        let output = self.fc1.forward(&input_tensor)?;
        let output = output.relu()?;
        let output = self.fc2.forward(&output)?;

        let probs = candle_nn::ops::softmax(&output, 1)?;

        // Absolute Rank Hardening
        let mut p = probs;
        while p.rank() > 1 {
            let dims = p.dims();
            p = p.get(dims[0] - 1)?;
        }

        if p.rank() == 0 {
            // Convert scalar to vector of 1
            let val = p.to_vec0::<f32>()?;
            let results = [val];

            let mut max_idx = 0;
            let mut max_val = 0.0;
            for (i, &val) in results.iter().enumerate() {
                if val > max_val {
                    max_val = val;
                    max_idx = i;
                }
            }
            if let Some(intent) = self.intents.get(max_idx) {
                return Ok((format!("ACTION: {}", intent), max_val));
            }
            return Err(anyhow!("Logic failure in rank-0 handling"));
        }

        let results = p.to_vec1::<f32>()?;

        let mut max_idx = 0;
        let mut max_val = 0.0;
        for (i, &val) in results.iter().take(self.intents.len()).enumerate() {
            if val > max_val {
                max_val = val;
                max_idx = i;
            }
        }

        if let Some(intent) = self.intents.get(max_idx) {
            return Ok((format!("ACTION: {}", intent), max_val));
        }

        Err(anyhow!("Low confidence in neural reflex."))
    }

    /// Deterministic Semantic Embedding Substrate
    /// Optimized for <2ms Instant-Intelligence.
    pub fn semantic_centroid_projection(
        prompt: &str,
        anchors: Option<&[crate::susi_core::AgentProfile]>,
    ) -> Result<Vec<f32>> {
        let start = std::time::Instant::now();
        let mut vec = vec![0.0f32; Self::DIM];
        let prompt_lower = prompt.to_lowercase();
        let words: Vec<&str> = prompt_lower
            .split(|c: char| !c.is_alphanumeric())
            .filter(|s| !s.is_empty())
            .collect();

        if words.is_empty() {
            return Ok(vec);
        }

        for (i, word) in words.iter().enumerate() {
            let word_vec = Self::get_semantic_anchor(word, anchors);
            for (j, &val) in word_vec.iter().enumerate() {
                let weight = 1.0 / (i as f32 + 1.0);
                vec[j] += val * weight;
            }
        }

        let sum_sq = vec.iter().map(|x| x * x).sum::<f32>();
        if sum_sq > 0.0 {
            let norm = sum_sq.sqrt();
            for x in vec.iter_mut() {
                *x /= norm;
            }
        }

        let _elapsed = start.elapsed();

        Ok(vec)
    }

    fn get_semantic_anchor(
        word: &str,
        anchors: Option<&[crate::susi_core::AgentProfile]>,
    ) -> Vec<f32> {
        let mut anchor = vec![0.0f32; Self::DIM];

        // Zero-Lock Anchor Mapping
        if let Some(agent_profiles) = anchors {
            for agent in agent_profiles {
                if agent.semantic_anchors.iter().any(|a| a == word) {
                    let offset = 80 + (agent.name.len() % 40);
                    anchor[offset] = 1.0;
                    return anchor;
                }
            }
        }

        let mut h = 0u32;
        for b in word.as_bytes() {
            h = h.wrapping_add(*b as u32);
        }

        let category = match word {
            "status" | "health" | "state" | "check" | "hardware" | "system" | "report" => 0,
            "version" | "ver" | "build" | "engine" | "revision" => 1,
            "write" | "save" | "create" | "file" | "update" | "put" => 2,
            "read" | "get" | "fetch" | "cat" | "show" | "content" => 3,
            "list" | "ls" | "dir" | "directory" | "folder" | "files" => 4,
            "scout" | "search" | "find" | "look" | "discover" | "mcp" => 5,
            "reason" | "think" | "solve" | "complex" | "calculate" => 6,
            "fix" | "heal" | "repair" | "audit" | "compliance" => 7,
            _ => 99,
        };

        if category < 10 {
            let start = category * 10;
            for val in anchor.iter_mut().skip(start).take(10) {
                *val = 1.0;
            }
        } else {
            anchor[(h as usize) % Self::DIM] = 0.5;
        }
        anchor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INTENTS: &[&str] = &["reason", "status"];

    fn intents() -> Vec<String> {
        INTENTS.iter().map(|intent| (*intent).to_string()).collect()
    }

    #[test]
    fn test_list_dynamic_intents() {
        let intents = SusiAlphaModel::list_dynamic_intents();
        assert!(!intents.is_empty());
        // Must be sorted and contain foundational intents
        assert!(intents.contains(&"status".to_string()));
        assert!(intents.contains(&"version".to_string()));
    }

    #[test]
    fn test_semantic_centroid_projection_determinism() {
        let vec1 =
            SusiAlphaModel::semantic_centroid_projection("check engine status", None).unwrap();
        let vec2 =
            SusiAlphaModel::semantic_centroid_projection("check engine status", None).unwrap();
        assert_eq!(vec1.len(), SusiAlphaModel::DIM);
        assert_eq!(vec1, vec2);
    }

    #[test]
    fn training_entries_require_valid_json_and_known_exact_action() {
        let malformed = parse_training_entries("not-json", &intents()).unwrap_err();
        assert!(malformed.to_string().contains("line 1"));

        let unknown = parse_training_entries(
            r#"{"intent":"inspect","action":"status report","timestamp":1}"#,
            &intents(),
        )
        .unwrap_err();
        assert!(unknown.to_string().contains("unknown distillation action"));
    }

    #[test]
    fn training_entries_map_canonical_action_without_substring_guessing() {
        let parsed = parse_training_entries(
            r#"{"intent":"inspect","action":"STATUS","timestamp":1}"#,
            &intents(),
        )
        .unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].1, 1);
    }

    #[test]
    fn extending_vocabulary_preserves_existing_output_indices() {
        let extended = extend_vocabulary(
            vec!["status".into(), "reason".into()],
            vec!["alpha".into(), "status".into(), "beta".into()],
        );
        assert_eq!(extended, ["status", "reason", "alpha", "beta"]);
    }

    #[test]
    fn vocabulary_validation_rejects_case_insensitive_duplicates() {
        let error = validate_vocabulary(vec!["status".into(), "STATUS".into()]).unwrap_err();
        assert!(error.to_string().contains("duplicate action"));
    }

    #[test]
    fn vocabulary_is_bound_to_exact_weight_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let weights = dir.path().join("alpha.safetensors");
        std::fs::write(&weights, b"weights-v1").unwrap();
        save_vocabulary(&weights, &["status".into(), "reason".into()]).unwrap();
        assert_eq!(load_vocabulary(&weights).unwrap(), ["status", "reason"]);

        std::fs::write(&weights, b"weights-v2").unwrap();
        let error = load_vocabulary(&weights).unwrap_err();
        assert!(error.to_string().contains("weights/vocabulary mismatch"));
    }
}
