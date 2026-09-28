// SUSI-Alpha: Native Neural Intelligence Substrate
// 100% Rust implementation using Candle for Tier 0 Reflex Distillation

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use susi_vendor_candle::candle_core::{DType, Tensor};
use susi_vendor_candle::candle_nn;
use susi_vendor_candle::candle_nn::{
    AdamW, Linear, Module, Optimizer, ParamsAdamW, VarBuilder, VarMap,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistillationStaged {
    pub intent: String,
    pub action: String,
    pub timestamp: u64,
}

#[derive(Debug)]
struct TrainingBatch {
    entries: Vec<(DistillationStaged, u32)>,
    skipped: usize,
}

/// Malformed or blank lines fail the batch. Well-formed records that cannot
/// be labeled (blank intent, action outside the vocabulary) are skipped and
/// counted: a restored claim would otherwise fail on them every cycle, and
/// once the vocabulary is full an unknown action never becomes trainable.
fn parse_training_entries(content: &str, dynamic_intents: &[String]) -> Result<TrainingBatch> {
    let mut entries = Vec::new();
    let mut skipped = 0usize;
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
            skipped += 1;
            continue;
        }
        let action = entry.action.trim();
        let Some(label) = dynamic_intents
            .iter()
            .position(|candidate| candidate.eq_ignore_ascii_case(action))
        else {
            skipped += 1;
            continue;
        };
        let label = u32::try_from(label).map_err(|_| anyhow!("intent label exceeds u32"))?;
        entries.push((entry, label));
    }
    if entries.is_empty() {
        if skipped > 0 {
            return Err(anyhow!(
                "No trainable distillation records: {skipped} skipped (blank intent or action outside the reflex vocabulary)."
            ));
        }
        return Err(anyhow!("Empty distillation dataset."));
    }
    if skipped > 0 {
        // Construction records the skip in the typed error-metrics sink.
        let _emit = crate::susi_error::EaiError::inference(format!(
            "skipped {skipped} untrainable distillation record(s)"
        ));
    }
    Ok(TrainingBatch { entries, skipped })
}

fn vocabulary_path(weights_path: &Path) -> PathBuf {
    weights_path.with_extension("intents.json")
}

const VOCABULARY_SCHEMA: &str = "susi/reflex-vocabulary/v1";
const BUNDLE_SCHEMA: &str = "susi/reflex-bundle/v1";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionVocabulary {
    schema: String,
    weights_sha256: String,
    intents: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReflexBundle {
    schema: String,
    weights_file: String,
    #[serde(default)]
    previous_weights_file: Option<String>,
}

fn bundle_path(configured_weights: &Path) -> PathBuf {
    configured_weights.with_extension("bundle.json")
}

fn local_checkpoint_path(configured_weights: &Path, filename: &str) -> Result<PathBuf> {
    let file = Path::new(filename);
    if file.file_name().and_then(|name| name.to_str()) != Some(filename) {
        return Err(anyhow!("reflex bundle contains a non-local weights path"));
    }
    let parent = configured_weights
        .parent()
        .ok_or_else(|| anyhow!("configured reflex weights have no parent"))?;
    Ok(parent.join(file))
}

fn resolve_checkpoint_candidates(configured_weights: &Path) -> Result<Vec<PathBuf>> {
    let manifest_path = bundle_path(configured_weights);
    if manifest_path.exists() {
        let bytes = std::fs::read(&manifest_path)?;
        let manifest: ReflexBundle = serde_json::from_slice(&bytes)?;
        if manifest.schema != BUNDLE_SCHEMA {
            return Err(anyhow!(
                "unsupported reflex bundle schema {:?}",
                manifest.schema
            ));
        }
        let mut candidates = vec![local_checkpoint_path(
            configured_weights,
            &manifest.weights_file,
        )?];
        if let Some(previous) = manifest.previous_weights_file {
            candidates.push(local_checkpoint_path(configured_weights, &previous)?);
        }
        return Ok(candidates);
    }
    Ok(configured_weights
        .is_file()
        .then(|| configured_weights.to_path_buf())
        .into_iter()
        .collect())
}

fn usable_checkpoint(configured_weights: &Path) -> Result<Option<(PathBuf, Vec<String>)>> {
    let candidates = resolve_checkpoint_candidates(configured_weights)?;
    let mut last_error = None;
    for candidate in &candidates {
        match load_vocabulary(candidate) {
            Ok(intents) => return Ok(Some((candidate.clone(), intents))),
            Err(error) => last_error = Some(error),
        }
    }
    if candidates.is_empty() {
        Ok(None)
    } else {
        Err(last_error.unwrap_or_else(|| anyhow!("no usable reflex checkpoint")))
    }
}

struct PublishOutcome {
    cleanup_failures: Vec<String>,
}

fn is_generated_checkpoint(filename: &str, stem: &str) -> bool {
    let Some(middle) = filename
        .strip_prefix(&format!("{stem}."))
        .and_then(|name| name.strip_suffix(".safetensors"))
    else {
        return false;
    };
    let mut parts = middle.split('.');
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(generation), Some(pid), None)
            if generation.parse::<u128>().is_ok() && pid.parse::<u32>().is_ok()
    )
}

fn prune_checkpoint_generations(configured_weights: &Path, keep: &[PathBuf]) -> Vec<String> {
    let Some(parent) = configured_weights.parent() else {
        return vec!["configured reflex weights have no parent".into()];
    };
    let Some(stem) = configured_weights
        .file_stem()
        .and_then(|stem| stem.to_str())
    else {
        return vec!["configured reflex weights have no UTF-8 stem".into()];
    };
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) => return vec![format!("scan reflex generations: {error}")],
    };
    let mut failures = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(filename) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !is_generated_checkpoint(filename, stem) || keep.iter().any(|kept| kept == &path) {
            continue;
        }
        if let Err(error) = std::fs::remove_file(&path) {
            failures.push(format!(
                "remove stale checkpoint {}: {error}",
                path.display()
            ));
            continue;
        }
        let vocabulary = vocabulary_path(&path);
        if let Err(error) = std::fs::remove_file(&vocabulary) {
            if error.kind() != std::io::ErrorKind::NotFound {
                failures.push(format!(
                    "remove stale checkpoint vocabulary {}: {error}",
                    vocabulary.display()
                ));
            }
        }
    }
    failures
}

fn publish_checkpoint(
    varmap: &VarMap,
    configured_weights: &Path,
    intents: &[String],
) -> Result<PublishOutcome> {
    let parent = configured_weights
        .parent()
        .ok_or_else(|| anyhow!("configured reflex weights have no parent"))?;
    std::fs::create_dir_all(parent)?;
    let stem = configured_weights
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| anyhow!("configured reflex weights have no UTF-8 stem"))?;
    let generation = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let previous = usable_checkpoint(configured_weights)?.map(|(path, _)| path);
    let filename = format!("{stem}.{generation}.{}.safetensors", std::process::id());
    let weights = parent.join(&filename);
    varmap.save(&weights)?;
    if let Err(error) = save_vocabulary(&weights, intents) {
        let _ = std::fs::remove_file(&weights);
        return Err(error);
    }
    let manifest = ReflexBundle {
        schema: BUNDLE_SCHEMA.into(),
        weights_file: filename,
        previous_weights_file: previous.as_ref().and_then(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
        }),
    };
    let bytes = serde_json::to_vec_pretty(&manifest)?;
    crate::susi_config::atomic_write_bytes(&bundle_path(configured_weights), &bytes)
        .map_err(|error| anyhow!("publish reflex bundle: {error}"))?;
    let mut keep = vec![weights];
    if let Some(previous) = previous {
        keep.push(previous);
    }
    Ok(PublishOutcome {
        cleanup_failures: prune_checkpoint_generations(configured_weights, &keep),
    })
}

/// A fingerprint that identifies one published bundle. Without a readable
/// bundle mtime there is nothing to detect a republish by, so never cache.
fn fingerprint_is_stable(fingerprint: &str) -> bool {
    fingerprint != "missing" && fingerprint != "unknown-mtime"
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

    /// The published reflex model, reloaded only when the bundle fingerprint
    /// changes. Callers on the reflex hot path would otherwise re-read the
    /// config and re-mmap the weights for every uncached prompt.
    pub fn cached(global_dir: &Path) -> Result<std::sync::Arc<Self>> {
        type Slot = Option<(PathBuf, String, std::sync::Arc<SusiAlphaModel>)>;
        static CACHE: std::sync::LazyLock<parking_lot::RwLock<Slot>> =
            std::sync::LazyLock::new(|| parking_lot::RwLock::new(None));
        let before = Self::get_model_fingerprint(global_dir);
        if let Some((dir, cached_fingerprint, model)) = CACHE.read().as_ref() {
            if dir == global_dir && *cached_fingerprint == before && fingerprint_is_stable(&before)
            {
                return Ok(model.clone());
            }
        }
        let model = std::sync::Arc::new(Self::load(global_dir)?);
        // Cache only when no publication raced the load (including the
        // bootstrap bundle `load` itself may publish); otherwise an older
        // model could be pinned under a newer fingerprint.
        let after = Self::get_model_fingerprint(global_dir);
        *CACHE.write() = (before == after && fingerprint_is_stable(&after))
            .then(|| (global_dir.to_path_buf(), after, model.clone()));
        Ok(model)
    }

    #[allow(unsafe_code)]
    pub fn load(global_dir: &Path) -> Result<Self> {
        let alpha_filename =
            crate::susi_sandbox::manager::SusiConfig::load(global_dir)?.alpha_weights_filename();
        let weights_path = global_dir.join("models").join(&alpha_filename);
        let device = crate::hardware::HardwareProfiler::get_candle_device();

        let candidates = resolve_checkpoint_candidates(&weights_path)?;
        if !candidates.is_empty() {
            let mut last_error = None;
            for active_weights in candidates {
                let attempt = (|| -> Result<Self> {
                    let intents = load_vocabulary(&active_weights)?;
                    // SAFETY: mmap of an immutable generation owned by the substrate.
                    let vb = unsafe {
                        VarBuilder::from_mmaped_safetensors(&[&active_weights], DType::F32, &device)
                    }?;
                    let fc1 = candle_nn::linear(Self::DIM, Self::DIM, vb.pp("reflex"))?;
                    let fc2 = candle_nn::linear(Self::DIM, Self::DIM, vb.pp("reflex_out"))?;
                    Ok(Self { fc1, fc2, intents })
                })();
                match attempt {
                    Ok(model) => return Ok(model),
                    Err(error) => last_error = Some(error),
                }
            }
            return Err(last_error.unwrap_or_else(|| anyhow!("no usable reflex checkpoint")));
        }

        // Initialize default weights only when no published model exists.
        let varmap = VarMap::new();
        let vb = VarBuilder::from_varmap(&varmap, DType::F32, &device);
        let fc1 = candle_nn::linear(Self::DIM, Self::DIM, vb.pp("reflex"))?;
        let fc2 = candle_nn::linear(Self::DIM, Self::DIM, vb.pp("reflex_out"))?;

        let models_dir = global_dir.join("models");
        std::fs::create_dir_all(&models_dir)?;
        let intents = validate_vocabulary(Self::list_dynamic_intents())?;
        publish_checkpoint(&varmap, &weights_path, &intents)?;

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
        let active_checkpoint = usable_checkpoint(&weights_path)?;
        let dynamic_intents = if let Some((_, intents)) = &active_checkpoint {
            extend_vocabulary(intents.clone(), discovered_intents)
        } else {
            validate_vocabulary(discovered_intents)?
        };

        let device = crate::hardware::HardwareProfiler::get_candle_device();
        let mut varmap = VarMap::new();
        let vb = VarBuilder::from_varmap(&varmap, DType::F32, &device);

        let fc1 = candle_nn::linear(Self::DIM, Self::DIM, vb.pp("reflex"))?;
        let fc2 = candle_nn::linear(Self::DIM, Self::DIM, vb.pp("reflex_out"))?;
        if let Some((active, _)) = active_checkpoint {
            varmap.load(active)?;
        }

        let mut opt = AdamW::new(varmap.all_vars(), ParamsAdamW::default())?;

        // Load Data and Map to Dynamic Surface
        let content = std::fs::read_to_string(staged_file)?;
        let mut samples = Vec::new();
        let mut labels = Vec::new();

        let batch = parse_training_entries(&content, &dynamic_intents)?;

        for (entry, label) in batch.entries {
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

        let publication = publish_checkpoint(&varmap, &weights_path, &dynamic_intents)?;

        let cleanup = if publication.cleanup_failures.is_empty() {
            String::new()
        } else {
            format!(
                " Cleanup warnings: {}.",
                publication.cleanup_failures.join("; ")
            )
        };
        let skipped = if batch.skipped == 0 {
            String::new()
        } else {
            format!(
                " Skipped {} untrainable record(s) (blank intent or action outside the reflex vocabulary).",
                batch.skipped
            )
        };
        Ok(format!("Native distillation complete. Trained cumulatively on {} samples with the dynamic intent surface.{skipped}{cleanup}", samples.len()))
    }

    pub fn get_model_fingerprint(global_dir: &Path) -> String {
        let alpha_filename = crate::susi_sandbox::manager::SusiConfig::load(global_dir)
            .unwrap_or_default()
            .alpha_weights_filename();
        let weights_path = global_dir.join("models").join(alpha_filename);
        if let Ok(meta) = std::fs::metadata(bundle_path(&weights_path)) {
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
        assert!(unknown
            .to_string()
            .contains("No trainable distillation records"));
        assert!(unknown.to_string().contains("1 skipped"));
    }

    #[test]
    fn training_entries_map_canonical_action_without_substring_guessing() {
        let parsed = parse_training_entries(
            r#"{"intent":"inspect","action":"STATUS","timestamp":1}"#,
            &intents(),
        )
        .unwrap();
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(parsed.entries[0].1, 1);
        assert_eq!(parsed.skipped, 0);
    }

    #[test]
    fn untrainable_records_are_skipped_without_rejecting_the_batch() {
        let content = [
            r#"{"intent":"inspect","action":"status","timestamp":1}"#,
            r#"{"intent":"inspect","action":"uninstalled_tool","timestamp":2}"#,
            r#"{"intent":"   ","action":"reason","timestamp":3}"#,
            r#"{"intent":"think","action":"Reason","timestamp":4}"#,
        ]
        .join("\n");
        let parsed = parse_training_entries(&content, &intents()).unwrap();
        assert_eq!(parsed.skipped, 2);
        let labels: Vec<u32> = parsed.entries.iter().map(|(_, label)| *label).collect();
        assert_eq!(labels, [1, 0]);

        let torn = format!("{content}\n{{\"intent\":\"torn");
        assert!(parse_training_entries(&torn, &intents())
            .unwrap_err()
            .to_string()
            .contains("line 5"));
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
    fn cached_model_is_reused_until_the_bundle_is_republished() {
        let dir = tempfile::tempdir().unwrap();
        // First call publishes the bootstrap bundle, so it cannot be cached.
        let bootstrap = SusiAlphaModel::cached(dir.path()).unwrap();
        let first = SusiAlphaModel::cached(dir.path()).unwrap();
        assert!(!std::sync::Arc::ptr_eq(&bootstrap, &first));
        let again = SusiAlphaModel::cached(dir.path()).unwrap();
        assert!(std::sync::Arc::ptr_eq(&first, &again));
        assert!(!fingerprint_is_stable("missing"));
        assert!(!fingerprint_is_stable("unknown-mtime"));
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

    #[test]
    fn bundle_manifest_resolves_only_local_immutable_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let configured = dir.path().join("alpha.safetensors");
        let active = dir.path().join("alpha.1.7.safetensors");
        std::fs::write(&active, b"checkpoint").unwrap();
        crate::susi_config::atomic_write_bytes(
            &bundle_path(&configured),
            br#"{"schema":"susi/reflex-bundle/v1","weights_file":"alpha.1.7.safetensors"}"#,
        )
        .unwrap();
        assert_eq!(
            resolve_checkpoint_candidates(&configured).unwrap(),
            [active]
        );

        crate::susi_config::atomic_write_bytes(
            &bundle_path(&configured),
            br#"{"schema":"susi/reflex-bundle/v1","weights_file":"../escape.safetensors"}"#,
        )
        .unwrap();
        assert!(resolve_checkpoint_candidates(&configured).is_err());
    }

    #[test]
    fn unusable_active_checkpoint_falls_back_to_previous_generation() {
        let dir = tempfile::tempdir().unwrap();
        let configured = dir.path().join("alpha.safetensors");
        let previous = dir.path().join("alpha.previous.safetensors");
        std::fs::write(&previous, b"previous").unwrap();
        save_vocabulary(&previous, &["status".into()]).unwrap();
        std::fs::write(dir.path().join("alpha.active.safetensors"), b"corrupt").unwrap();
        crate::susi_config::atomic_write_bytes(
            &bundle_path(&configured),
            br#"{"schema":"susi/reflex-bundle/v1","weights_file":"alpha.active.safetensors","previous_weights_file":"alpha.previous.safetensors"}"#,
        )
        .unwrap();

        let (resolved, intents) = usable_checkpoint(&configured).unwrap().unwrap();
        assert_eq!(resolved, previous);
        assert_eq!(intents, ["status"]);
    }

    #[test]
    fn generation_pruning_keeps_active_previous_and_unrelated_files() {
        let dir = tempfile::tempdir().unwrap();
        let configured = dir.path().join("alpha.safetensors");
        let active = dir.path().join("alpha.3.7.safetensors");
        let previous = dir.path().join("alpha.2.7.safetensors");
        let stale = dir.path().join("alpha.1.7.safetensors");
        let unrelated = dir.path().join("alpha.custom.safetensors");
        for path in [&active, &previous, &stale, &unrelated] {
            std::fs::write(path, b"weights").unwrap();
        }
        std::fs::write(vocabulary_path(&stale), b"vocab").unwrap();

        assert!(
            prune_checkpoint_generations(&configured, &[active.clone(), previous.clone()])
                .is_empty()
        );
        assert!(active.exists());
        assert!(previous.exists());
        assert!(!stale.exists());
        assert!(!vocabulary_path(&stale).exists());
        assert!(unrelated.exists());
    }
}
