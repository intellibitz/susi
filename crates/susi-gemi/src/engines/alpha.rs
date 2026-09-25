// SUSI-Alpha: Native Neural Intelligence Substrate
// 100% Rust implementation using Candle for Tier 0 Reflex Distillation

use anyhow::{anyhow, Result};
use candle_core::{DType, Tensor};
use candle_nn::{AdamW, Linear, Module, Optimizer, ParamsAdamW, VarBuilder, VarMap};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistillationStaged {
    pub intent: String,
    pub action: String,
    pub timestamp: u64,
}

/// SUSI-Alpha Intent Classifier (Neural Reflex)
pub struct SusiAlphaModel {
    fc1: Linear,
    fc2: Linear,
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
                Self { fc1, fc2 }
            })
        })
    }

    #[allow(unsafe_code)]
    pub fn load(global_dir: &Path) -> Result<Self> {
        let alpha_filename = crate::susi_sandbox::manager::SusiConfig::load(global_dir)
            .unwrap_or_default()
            .alpha_weights_filename();
        let weights_path = global_dir.join("models").join(&alpha_filename);
        let device = crate::hardware::HardwareProfiler::get_candle_device();

        if weights_path.exists() {
            // SAFETY: mmap of a weights file the substrate owns; it is not modified while mapped.
            if let Ok(vb) = unsafe {
                VarBuilder::from_mmaped_safetensors(&[&weights_path], DType::F32, &device)
            } {
                if let (Ok(fc1), Ok(fc2)) = (
                    candle_nn::linear(Self::DIM, Self::DIM, vb.pp("reflex")),
                    candle_nn::linear(Self::DIM, Self::DIM, vb.pp("reflex_out"))
                        .or_else(|_| candle_nn::linear(Self::DIM, Self::DIM, vb.pp("reflex"))),
                ) {
                    return Ok(Self { fc1, fc2 });
                }
            }
        }

        // Initialize default weights if file missing or unreadable
        let varmap = VarMap::new();
        let vb = VarBuilder::from_varmap(&varmap, DType::F32, &device);
        let fc1 = candle_nn::linear(Self::DIM, Self::DIM, vb.pp("reflex"))?;
        let fc2 = candle_nn::linear(Self::DIM, Self::DIM, vb.pp("reflex_out"))?;

        let models_dir = global_dir.join("models");
        let _ = std::fs::create_dir_all(&models_dir);
        let _ = varmap.save(&weights_path);

        Ok(Self { fc1, fc2 })
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
        let staged_file = global_dir.join("distillation_staged.jsonl");
        if !staged_file.exists() {
            return Err(anyhow!("No staged distillation data found."));
        }

        let device = crate::hardware::HardwareProfiler::get_candle_device();
        let varmap = VarMap::new();
        let vb = VarBuilder::from_varmap(&varmap, DType::F32, &device);

        let fc1 = candle_nn::linear(Self::DIM, Self::DIM, vb.pp("reflex"))?;
        let fc2 = candle_nn::linear(Self::DIM, Self::DIM, vb.pp("reflex_out"))?;

        let mut opt = AdamW::new(varmap.all_vars(), ParamsAdamW::default())?;

        // Load Data and Map to Dynamic Surface
        let content = std::fs::read_to_string(&staged_file)?;
        let mut samples = Vec::new();
        let mut labels = Vec::new();

        let dynamic_intents = Self::list_dynamic_intents();

        for line in content.lines() {
            if let Ok(entry) = serde_json::from_str::<DistillationStaged>(line) {
                let vec = Self::semantic_centroid_projection(&entry.intent, None)?;
                samples.push(Tensor::from_vec(vec, (1, Self::DIM), &device)?);

                let action_clean = entry.action.to_lowercase();
                let label_idx = dynamic_intents
                    .iter()
                    .position(|i| action_clean.contains(&i.to_lowercase()))
                    .unwrap_or(dynamic_intents.len() - 1) as u32;
                labels.push(label_idx);
            }
        }

        // Neural Seeding (Synthetic Priming): Ensure new tools have at least one sample
        for (idx, intent) in dynamic_intents.iter().enumerate() {
            let vec = Self::semantic_centroid_projection(intent, None)?;
            samples.push(Tensor::from_vec(vec, (1, Self::DIM), &device)?);
            labels.push(idx as u32);
        }

        if samples.is_empty() {
            return Err(anyhow!("Empty distillation dataset."));
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
        let alpha_filename = crate::susi_sandbox::manager::SusiConfig::load(global_dir)
            .unwrap_or_default()
            .alpha_weights_filename();
        let weights_path = global_dir.join("models").join(&alpha_filename);
        let tmp_path = weights_path.with_extension("tmp");
        varmap.save(&tmp_path)?;
        std::fs::rename(tmp_path, weights_path)?;

        Ok(format!("Autonomous Distillation Complete. Retrained on {} samples with Dynamic Intent Surface.", samples.len()))
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
            let dynamic_intents = Self::list_dynamic_intents();
            if let Some(intent) = dynamic_intents.get(max_idx) {
                return Ok((format!("ACTION: {}", intent), max_val));
            }
            return Err(anyhow!("Logic failure in rank-0 handling"));
        }

        let results = p.to_vec1::<f32>()?;

        let mut max_idx = 0;
        let mut max_val = 0.0;
        for (i, &val) in results.iter().enumerate() {
            if val > max_val {
                max_val = val;
                max_idx = i;
            }
        }

        let dynamic_intents = Self::list_dynamic_intents();
        if let Some(intent) = dynamic_intents.get(max_idx) {
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
}
