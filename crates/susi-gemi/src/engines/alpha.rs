// SUSI-Alpha: Native Neural Intelligence Substrate
// 100% Rust implementation using Candle for Tier 0 Reflex Distillation

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use susi_error::{eai_err as anyhow, EaiResult as Result};
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
    /// Writer-supplied context. Mission-trace samples carry the mission's
    /// `outcome`; receipt samples (successful tool calls) carry none.
    #[serde(default)]
    pub performance_metadata: Option<serde_json::Value>,
}

/// A sample whose mission outcome is recorded and is not a success teaches
/// the classifier to repeat a failure; it is never trainable. Mirrors
/// `MissionTrace::succeeded`.
fn failed_outcome(entry: &DistillationStaged) -> bool {
    entry
        .performance_metadata
        .as_ref()
        .and_then(|meta| meta.get("outcome"))
        .and_then(|outcome| outcome.as_str())
        .is_some_and(|outcome| !matches!(outcome, "SUCCESS" | "COMPLETE"))
}

#[derive(Debug)]
struct TrainingBatch {
    entries: Vec<(DistillationStaged, u32)>,
    skipped: usize,
}

/// Malformed or blank lines fail the batch. Well-formed records that cannot
/// be labeled (blank intent, failed mission outcome, action outside the
/// vocabulary) are skipped and counted: a restored claim would otherwise
/// fail on them every cycle, and once the vocabulary is full an unknown
/// action never becomes trainable.
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
        if entry.intent.trim().is_empty() || failed_outcome(&entry) {
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
                "No trainable distillation records: {skipped} skipped (blank intent, failed mission outcome, or action outside the reflex vocabulary)."
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
    varmap
        .save(&weights)
        .map_err(crate::engines::candle_err::from_candle)?;
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

/// Prior samples replayed into every cycle, newest kept. Fine-tuning on only
/// the claimed batch would overwrite what earlier batches taught.
const REPLAY_CAPACITY: usize = 2048;

/// One trained sample, stored by action *name* so it survives vocabulary
/// growth; it is dropped on load if its action has left the vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ReplaySample {
    intent: String,
    action: String,
}

fn replay_path(weights_path: &Path) -> PathBuf {
    weights_path.with_extension("replay.jsonl")
}

/// Missing file = empty history; torn or foreign lines are skipped, since
/// the replay set is advisory training data, not a ledger.
fn load_replay(weights_path: &Path) -> Result<Vec<ReplaySample>> {
    match std::fs::read_to_string(replay_path(weights_path)) {
        Ok(text) => Ok(text
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(anyhow!("read reflex replay set: {error}")),
    }
}

/// Newer samples win: an intent restaged with a different action replaces
/// the old label (the operator's latest verified behavior). Keeps the most
/// recent `REPLAY_CAPACITY` distinct intents, oldest first.
fn merge_replay(prior: Vec<ReplaySample>, newer: Vec<ReplaySample>) -> Vec<ReplaySample> {
    let mut seen = std::collections::HashSet::new();
    let mut merged: Vec<ReplaySample> = prior
        .into_iter()
        .chain(newer)
        .rev()
        .filter(|sample| seen.insert(replay_key(&sample.intent)))
        .take(REPLAY_CAPACITY)
        .collect();
    merged.reverse();
    merged
}

fn replay_key(intent: &str) -> String {
    intent.trim().to_lowercase()
}

fn save_replay(weights_path: &Path, samples: &[ReplaySample]) -> Result<()> {
    let mut body = Vec::new();
    for sample in samples {
        body.extend(serde_json::to_vec(sample)?);
        body.push(b'\n');
    }
    crate::susi_config::atomic_write_bytes(&replay_path(weights_path), &body)
        .map_err(|error| anyhow!("persist reflex replay set: {error}"))
}

/// One in `HOLDOUT_BUCKETS` staged intents is held out of the candidate fit
/// and used to judge it against the active checkpoint.
const HOLDOUT_BUCKETS: u64 = 5;
/// Fewer held-out samples than this is too noisy to gate publication on.
const MIN_HOLDOUT: usize = 3;
const TRAINING_EPOCHS: usize = 100;

/// Stable across cycles (hash of the normalized intent, not of its position
/// in a batch): a held-out sample is always held out, so the candidate fit
/// never sees it, while the published refit does. Replayed held-out samples
/// therefore measure whether a candidate *forgets* what the active model
/// learned.
fn is_holdout(intent: &str) -> bool {
    let digest = Sha256::digest(intent.trim().to_lowercase().as_bytes());
    let mut head = [0u8; 8];
    head.copy_from_slice(&digest[..8]);
    u64::from_le_bytes(head) % HOLDOUT_BUCKETS == 0
}

/// The two-layer reflex network plus the `VarMap` that owns its weights.
struct ReflexNet {
    varmap: VarMap,
    fc1: Linear,
    fc2: Linear,
}

impl ReflexNet {
    /// Fresh weights, overwritten by `checkpoint` when one is given.
    fn init(
        checkpoint: Option<&Path>,
        device: &susi_vendor_candle::candle_core::Device,
    ) -> Result<Self> {
        let mut varmap = VarMap::new();
        let vb = VarBuilder::from_varmap(&varmap, DType::F32, device);
        let fc1 = candle_nn::linear(SusiAlphaModel::DIM, SusiAlphaModel::DIM, vb.pp("reflex"))
            .map_err(crate::engines::candle_err::from_candle)?;
        let fc2 = candle_nn::linear(
            SusiAlphaModel::DIM,
            SusiAlphaModel::DIM,
            vb.pp("reflex_out"),
        )
        .map_err(crate::engines::candle_err::from_candle)?;
        if let Some(checkpoint) = checkpoint {
            varmap
                .load(checkpoint)
                .map_err(crate::engines::candle_err::from_candle)?;
        }
        Ok(Self { varmap, fc1, fc2 })
    }

    fn logits(&self, x: &Tensor) -> Result<Tensor> {
        let hidden = self
            .fc1
            .forward(x)
            .map_err(crate::engines::candle_err::from_candle)?
            .relu()
            .map_err(crate::engines::candle_err::from_candle)?;
        self.fc2
            .forward(&hidden)
            .map_err(crate::engines::candle_err::from_candle)
    }

    fn fit(&self, x: &Tensor, y: &Tensor) -> Result<()> {
        let mut opt = AdamW::new(self.varmap.all_vars(), ParamsAdamW::default())
            .map_err(crate::engines::candle_err::from_candle)?;
        for _epoch in 0..TRAINING_EPOCHS {
            let log_sm = candle_nn::ops::log_softmax(&self.logits(x)?, 1)
                .map_err(crate::engines::candle_err::from_candle)?;
            let loss = candle_nn::loss::nll(&log_sm, y)
                .map_err(crate::engines::candle_err::from_candle)?;
            opt.backward_step(&loss)
                .map_err(crate::engines::candle_err::from_candle)?;
        }
        Ok(())
    }

    /// Samples whose argmax over the first `vocabulary` outputs equals the
    /// label. A label the vocabulary cannot express counts as a miss.
    fn correct(&self, x: &Tensor, labels: &[u32], vocabulary: usize) -> Result<usize> {
        let rows = self
            .logits(x)?
            .to_vec2::<f32>()
            .map_err(crate::engines::candle_err::from_candle)?;
        Ok(rows
            .iter()
            .zip(labels)
            .filter(|(row, &label)| {
                let predicted = row
                    .iter()
                    .take(vocabulary)
                    .enumerate()
                    .max_by(|a, b| a.1.total_cmp(b.1))
                    .map(|(index, _)| index);
                predicted == usize::try_from(label).ok()
            })
            .count())
    }
}

/// A staged or primed training sample: intent text and its action label.
type Labeled<'a> = (&'a str, u32);

/// Projects `texts` into one `(n, DIM)` batch.
fn projection_batch(
    texts: &[&str],
    device: &susi_vendor_candle::candle_core::Device,
) -> Result<Tensor> {
    let mut flat = Vec::with_capacity(texts.len() * SusiAlphaModel::DIM);
    for text in texts {
        flat.extend(SusiAlphaModel::reflex_features(text));
    }
    Tensor::from_vec(flat, (texts.len(), SusiAlphaModel::DIM), device)
        .map_err(crate::engines::candle_err::from_candle)
}

fn labeled_batch(
    samples: &[(&str, u32)],
    device: &susi_vendor_candle::candle_core::Device,
) -> Result<(Tensor, Tensor)> {
    let texts: Vec<&str> = samples.iter().map(|(text, _)| *text).collect();
    let labels: Vec<u32> = samples.iter().map(|(_, label)| *label).collect();
    let x = projection_batch(&texts, device)?;
    let y = Tensor::from_vec(labels, samples.len(), device)
        .map_err(crate::engines::candle_err::from_candle)?;
    Ok((x, y))
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
        let device = crate::models::hardware::HardwareProfiler::get_candle_device();

        let candidates = resolve_checkpoint_candidates(&weights_path)?;
        if !candidates.is_empty() {
            let mut last_error = None;
            for active_weights in candidates {
                let attempt = (|| -> Result<Self> {
                    let intents = load_vocabulary(&active_weights)?;
                    // SAFETY: mmap of an immutable generation owned by the substrate.
                    let vb = unsafe {
                        VarBuilder::from_mmaped_safetensors(&[&active_weights], DType::F32, &device)
                    }
                    .map_err(crate::engines::candle_err::from_candle)?;
                    let fc1 = candle_nn::linear(Self::DIM, Self::DIM, vb.pp("reflex"))
                        .map_err(crate::engines::candle_err::from_candle)?;
                    let fc2 = candle_nn::linear(Self::DIM, Self::DIM, vb.pp("reflex_out"))
                        .map_err(crate::engines::candle_err::from_candle)?;
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
        let fc1 = candle_nn::linear(Self::DIM, Self::DIM, vb.pp("reflex"))
            .map_err(crate::engines::candle_err::from_candle)?;
        let fc2 = candle_nn::linear(Self::DIM, Self::DIM, vb.pp("reflex_out"))
            .map_err(crate::engines::candle_err::from_candle)?;

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

        let device = crate::models::hardware::HardwareProfiler::get_candle_device();
        let content = std::fs::read_to_string(staged_file)?;
        let batch = parse_training_entries(&content, &dynamic_intents)?;
        let fresh: Vec<ReplaySample> = batch
            .entries
            .iter()
            .map(|(entry, label)| ReplaySample {
                intent: entry.intent.clone(),
                action: dynamic_intents
                    .get(*label as usize)
                    .cloned()
                    .unwrap_or_else(|| entry.action.clone()),
            })
            .collect();
        let prior = load_replay(&weights_path)?;
        let replayed = merge_replay(prior, fresh);
        let label_of = |action: &str| {
            dynamic_intents
                .iter()
                .position(|candidate| candidate.eq_ignore_ascii_case(action))
                .and_then(|index| u32::try_from(index).ok())
        };
        let staged: Vec<(&str, u32)> = replayed
            .iter()
            .filter_map(|sample| Some((sample.intent.as_str(), label_of(&sample.action)?)))
            .collect();
        let fresh_keys: std::collections::HashSet<String> = batch
            .entries
            .iter()
            .map(|(entry, _)| replay_key(&entry.intent))
            .collect();
        let replay_count = staged
            .iter()
            .filter(|(intent, _)| !fresh_keys.contains(&replay_key(intent)))
            .count();
        // Synthetic priming: every vocabulary action gets at least one sample,
        // so a newly installed tool is reachable before anyone has used it.
        let mut primes = Vec::with_capacity(dynamic_intents.len());
        for (index, intent) in dynamic_intents.iter().enumerate() {
            let label = u32::try_from(index).map_err(|_| anyhow!("intent label exceeds u32"))?;
            primes.push((intent.as_str(), label));
        }

        let active = active_checkpoint
            .as_ref()
            .map(|(path, intents)| (path.as_path(), intents.len()));
        let gate = Self::holdout_gate(active, &staged, &primes, &dynamic_intents, &device)?;

        let net = ReflexNet::init(active.map(|(path, _)| path), &device)?;
        let all: Vec<(&str, u32)> = staged.iter().chain(&primes).copied().collect();
        let (x, y) = labeled_batch(&all, &device)?;
        net.fit(&x, &y)?;
        let samples = all.len();

        let publication = publish_checkpoint(&net.varmap, &weights_path, &dynamic_intents)?;
        // After publication: a failed write loses only this cycle's additions
        // to the replay set, never the checkpoint.
        let replay_note = match save_replay(&weights_path, &replayed) {
            Ok(()) => format!(" Replayed {replay_count} prior sample(s)."),
            Err(error) => format!(" Replayed {replay_count} prior sample(s); {error}."),
        };

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
                " Skipped {} untrainable record(s) (blank intent, failed mission outcome, or action outside the reflex vocabulary).",
                batch.skipped
            )
        };
        Ok(format!("Native distillation complete. Trained cumulatively on {samples} samples with the dynamic intent surface.{replay_note}{gate}{skipped}{cleanup}"))
    }

    /// Publication gate. A candidate fit from the active weights on the
    /// non-held-out samples must predict the held-out ones at least as well
    /// as the active checkpoint does, or nothing is published: the error
    /// makes `ReflexTrainer` restore the claim, so the samples are retried
    /// with more data rather than dropped. Returns the report fragment.
    fn holdout_gate(
        active: Option<(&Path, usize)>,
        staged: &[(&str, u32)],
        primes: &[(&str, u32)],
        vocabulary: &[String],
        device: &susi_vendor_candle::candle_core::Device,
    ) -> Result<String> {
        let Some((checkpoint, active_vocabulary)) = active else {
            return Ok(" Held-out gate skipped: no active checkpoint to regress against.".into());
        };
        let (holdout, train): (Vec<Labeled>, Vec<Labeled>) =
            staged.iter().partition(|(intent, _)| is_holdout(intent));
        if holdout.len() < MIN_HOLDOUT {
            return Ok(format!(
                " Held-out gate skipped: {} held-out sample(s), need {MIN_HOLDOUT}.",
                holdout.len()
            ));
        }
        let held_texts: Vec<&str> = holdout.iter().map(|(intent, _)| *intent).collect();
        let hx = projection_batch(&held_texts, device)?;
        let held_labels: Vec<u32> = holdout.iter().map(|(_, label)| *label).collect();
        let baseline = ReflexNet::init(Some(checkpoint), device)?.correct(
            &hx,
            &held_labels,
            active_vocabulary,
        )?;
        let candidate = ReflexNet::init(Some(checkpoint), device)?;
        let fit: Vec<(&str, u32)> = train.iter().chain(primes).copied().collect();
        let (x, y) = labeled_batch(&fit, device)?;
        candidate.fit(&x, &y)?;
        let scored = candidate.correct(&hx, &held_labels, vocabulary.len())?;
        let n = holdout.len();
        if scored < baseline {
            return Err(anyhow!(
                "reflex checkpoint held back: held-out accuracy would regress from {baseline}/{n} (active) to {scored}/{n}; active checkpoint kept"
            ));
        }
        Ok(format!(
            " Held-out gate passed: {scored}/{n} vs active {baseline}/{n}."
        ))
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
        let device = crate::models::hardware::HardwareProfiler::get_candle_device();
        let input_vec = Self::reflex_features(prompt);
        let input_tensor = Tensor::from_vec(input_vec, (1, Self::DIM), &device)
            .map_err(crate::engines::candle_err::from_candle)?;

        let output = self
            .fc1
            .forward(&input_tensor)
            .map_err(crate::engines::candle_err::from_candle)?;
        let output = output
            .relu()
            .map_err(crate::engines::candle_err::from_candle)?;
        let output = self
            .fc2
            .forward(&output)
            .map_err(crate::engines::candle_err::from_candle)?;

        let probs =
            candle_nn::ops::softmax(&output, 1).map_err(crate::engines::candle_err::from_candle)?;

        // Absolute Rank Hardening
        let mut p = probs;
        while p.rank() > 1 {
            let dims = p.dims();
            p = p
                .get(dims[0] - 1)
                .map_err(crate::engines::candle_err::from_candle)?;
        }

        if p.rank() == 0 {
            // Convert scalar to vector of 1
            let val = p
                .to_vec0::<f32>()
                .map_err(crate::engines::candle_err::from_candle)?;
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

        let results = p
            .to_vec1::<f32>()
            .map_err(crate::engines::candle_err::from_candle)?;

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

        if let Some(category) = anchor_category(word) {
            for val in anchor.iter_mut().skip(category * 10).take(10) {
                *val = 1.0;
            }
        } else {
            anchor[(fnv1a(word, 0) as usize) % Self::DIM] = 0.5;
        }
        anchor
    }

    /// The reflex classifier's input features. `semantic_centroid_projection`
    /// is tuned for fleet recruitment (a category word lights a 10-dim band
    /// at 1.0 while any other word lights one dim at 0.5, first word weighted
    /// most), and under it a leading category word decides everything:
    /// "write a poem about the ocean" projected within 0.01 of "write notes
    /// to todo.md" and was served `write_file` at 0.84 confidence. Here
    /// stopwords are dropped and every remaining word contributes equal
    /// norm, position-independent — a category word spread over its band,
    /// any other word over two FNV-1a buckets — so content words count as
    /// much as the verb.
    pub fn reflex_features(prompt: &str) -> Vec<f32> {
        let mut vec = vec![0.0f32; Self::DIM];
        let lower = prompt.to_lowercase();
        for word in lower
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty() && !REFLEX_STOPWORDS.contains(w))
        {
            if let Some(category) = anchor_category(word) {
                let weight = 1.0 / (10f32).sqrt();
                for val in vec.iter_mut().skip(category * 10).take(10) {
                    *val += weight;
                }
            } else {
                let weight = std::f32::consts::FRAC_1_SQRT_2;
                vec[(fnv1a(word, 0) as usize) % Self::DIM] += weight;
                vec[(fnv1a(word, 1) as usize) % Self::DIM] += weight;
            }
        }
        let norm = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            vec.iter_mut().for_each(|x| *x /= norm);
        }
        vec
    }
}

/// Function words that carry no intent; dropped from reflex features so a
/// shared "the"/"a" never makes two unrelated prompts look alike.
const REFLEX_STOPWORDS: &[&str] = &[
    "a", "an", "the", "and", "or", "of", "to", "in", "on", "at", "for", "with", "about", "into",
    "from", "by", "is", "are", "was", "be", "it", "this", "that", "these", "those", "me", "my",
    "i", "you", "your", "please", "can", "could", "would", "what", "which", "who", "how", "do",
    "does",
];

/// The 10-dim band a known intent word lights, if any.
fn anchor_category(word: &str) -> Option<usize> {
    match word {
        "status" | "health" | "state" | "check" | "hardware" | "system" | "report" => Some(0),
        "version" | "ver" | "build" | "engine" | "revision" => Some(1),
        "write" | "save" | "create" | "file" | "update" | "put" => Some(2),
        "read" | "get" | "fetch" | "cat" | "show" | "content" => Some(3),
        "list" | "ls" | "dir" | "directory" | "folder" | "files" => Some(4),
        "scout" | "search" | "find" | "look" | "discover" | "mcp" => Some(5),
        "reason" | "think" | "solve" | "complex" | "calculate" => Some(6),
        "fix" | "heal" | "repair" | "audit" | "compliance" => Some(7),
        _ => None,
    }
}

/// FNV-1a with a seed byte folded in first, so seeds 0 and 1 give two
/// independent buckets. The word hash was a byte *sum* before EV-CLAUDE-004,
/// which sent every anagram ("stop"/"post"/"pots"/"tops") to one bucket.
fn fnv1a(word: &str, seed: u8) -> u32 {
    let mut h = 0x811c_9dc5u32;
    for b in std::iter::once(seed).chain(word.bytes()) {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    const INTENTS: &[&str] = &["reason", "status"];

    fn intents() -> Vec<String> {
        INTENTS.iter().map(|intent| (*intent).to_string()).collect()
    }

    /// Phrases built only from category-0 anchor words ("status", "health",
    /// ...) all project to the same vector, so a label taught on the train
    /// split transfers exactly to the held-out split.
    fn status_phrases() -> Vec<String> {
        let words = [
            "status", "health", "state", "check", "hardware", "system", "report",
        ];
        let mut phrases = Vec::new();
        for a in words {
            for b in words {
                for c in words {
                    phrases.push(format!("{a} {b} {c}"));
                }
            }
        }
        phrases
    }

    fn stage(path: &Path, samples: &[(&str, &str)]) {
        let body: String = samples
            .iter()
            .map(|(intent, action)| {
                format!(
                    "{}\n",
                    serde_json::json!({"intent": intent, "action": action, "timestamp": 1})
                )
            })
            .collect();
        std::fs::write(path, body).unwrap();
    }

    fn active_weights(global_dir: &Path) -> PathBuf {
        let filename = crate::susi_sandbox::manager::SusiConfig::load(global_dir)
            .unwrap()
            .alpha_weights_filename();
        usable_checkpoint(&global_dir.join("models").join(filename))
            .unwrap()
            .unwrap()
            .0
    }

    fn sample(intent: &str, action: &str) -> ReplaySample {
        ReplaySample {
            intent: intent.into(),
            action: action.into(),
        }
    }

    #[test]
    fn replay_merge_keeps_newest_label_and_bounded_recency() {
        let merged = merge_replay(
            vec![sample("check status", "status"), sample("why", "reason")],
            vec![sample("  CHECK Status ", "reason"), sample("new", "status")],
        );
        assert_eq!(
            merged,
            [
                sample("why", "reason"),
                sample("  CHECK Status ", "reason"),
                sample("new", "status")
            ]
        );

        let many: Vec<ReplaySample> = (0..REPLAY_CAPACITY + 10)
            .map(|i| sample(&format!("intent {i}"), "status"))
            .collect();
        let capped = merge_replay(many, Vec::new());
        assert_eq!(capped.len(), REPLAY_CAPACITY);
        assert_eq!(capped[0].intent, "intent 10", "oldest are evicted first");
    }

    #[test]
    fn replay_load_tolerates_missing_and_torn_files() {
        let dir = tempfile::tempdir().unwrap();
        let weights = dir.path().join("alpha.safetensors");
        assert!(load_replay(&weights).unwrap().is_empty());
        save_replay(&weights, &[sample("a", "status")]).unwrap();
        let mut body = std::fs::read(replay_path(&weights)).unwrap();
        body.extend_from_slice(b"{\"intent\":\"torn");
        std::fs::write(replay_path(&weights), body).unwrap();
        assert_eq!(load_replay(&weights).unwrap(), [sample("a", "status")]);
    }

    #[test]
    fn prior_cycles_are_replayed_into_later_training() {
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join("staged.jsonl");
        let first = ["zorblat quux", "zorblat flim", "quux flim zorblat"];
        stage(&staged, &first.map(|intent| (intent, "reason")));
        SusiAlphaModel::train_on_staged_file(dir.path(), &staged).unwrap();

        stage(&staged, &[("show the content", "read_file")]);
        let report = SusiAlphaModel::train_on_staged_file(dir.path(), &staged).unwrap();
        assert!(report.contains("Replayed 3 prior sample(s)"), "{report}");

        let filename = crate::susi_sandbox::manager::SusiConfig::load(dir.path())
            .unwrap()
            .alpha_weights_filename();
        let replay = load_replay(&dir.path().join("models").join(filename)).unwrap();
        let intents: Vec<&str> = replay.iter().map(|s| s.intent.as_str()).collect();
        assert_eq!(intents, [first[0], first[1], first[2], "show the content"]);
        let (action, _) = SusiAlphaModel::load(dir.path())
            .unwrap()
            .predict_intent_with_confidence("zorblat quux")
            .unwrap();
        assert_eq!(
            action, "ACTION: reason",
            "cycle-1 knowledge survives cycle 2"
        );
    }

    #[test]
    fn holdout_split_is_stable_and_about_one_in_five() {
        let phrases = status_phrases();
        let held = phrases.iter().filter(|p| is_holdout(p)).count();
        assert!(
            (phrases.len() / 10..phrases.len() * 3 / 10).contains(&held),
            "{held} of {}",
            phrases.len()
        );
        for phrase in &phrases {
            assert_eq!(
                is_holdout(phrase),
                is_holdout(&format!("  {}  ", phrase.to_uppercase()))
            );
        }
    }

    #[test]
    fn regressing_checkpoint_is_held_back_and_improving_one_publishes() {
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join("staged.jsonl");
        let phrases = status_phrases();
        let (held, train): (Vec<&String>, Vec<&String>) =
            phrases.iter().partition(|p| is_holdout(p));
        let held: Vec<&str> = held.iter().take(6).map(|p| p.as_str()).collect();
        let train: Vec<&str> = train.iter().take(40).map(|p| p.as_str()).collect();

        // Cycle 1: no active checkpoint, so nothing to regress against.
        let teach: Vec<(&str, &str)> = held.iter().chain(&train).map(|p| (*p, "status")).collect();
        stage(&staged, &teach);
        let report = SusiAlphaModel::train_on_staged_file(dir.path(), &staged).unwrap();
        assert!(report.contains("no active checkpoint"), "{report}");
        let first = active_weights(dir.path());

        // Cycle 2: the train split now contradicts what the active model
        // gets right on the held-out split. Publication must be refused.
        let contradict: Vec<(&str, &str)> = held
            .iter()
            .map(|p| (*p, "status"))
            .chain(train.iter().map(|p| (*p, "reason")))
            .collect();
        stage(&staged, &contradict);
        let error = SusiAlphaModel::train_on_staged_file(dir.path(), &staged).unwrap_err();
        assert!(error.to_string().contains("held back"), "{error}");
        assert_eq!(
            active_weights(dir.path()),
            first,
            "active checkpoint must be kept"
        );

        // Cycle 3: consistent data passes the gate and publishes.
        stage(&staged, &teach);
        let report = SusiAlphaModel::train_on_staged_file(dir.path(), &staged).unwrap();
        assert!(report.contains("Held-out gate passed"), "{report}");
        assert_ne!(active_weights(dir.path()), first);
    }

    #[test]
    fn prediction_scoring_counts_only_the_expressible_vocabulary() {
        let device = susi_vendor_candle::candle_core::Device::Cpu;
        let net = ReflexNet::init(None, &device).unwrap();
        let x = projection_batch(&["status check"], &device).unwrap();
        // Whatever the random net predicts, an empty vocabulary predicts
        // nothing and a label beyond it can never count as correct.
        assert_eq!(net.correct(&x, &[0], 0).unwrap(), 0);
        let all = net.correct(&x, &[0], SusiAlphaModel::DIM).unwrap()
            + (1..SusiAlphaModel::DIM as u32)
                .map(|label| net.correct(&x, &[label], SusiAlphaModel::DIM).unwrap())
                .sum::<usize>();
        assert_eq!(all, 1, "exactly one label is the argmax");
    }

    #[test]
    fn test_list_dynamic_intents() {
        let intents = SusiAlphaModel::list_dynamic_intents();
        assert!(!intents.is_empty());
        // Must be sorted and contain foundational intents
        assert!(intents.contains(&"status".to_string()));
        assert!(intents.contains(&"version".to_string()));
    }

    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| x * y).sum()
    }

    /// Everyday training data shared by the out-of-distribution tests.
    const EVERYDAY: &[(&str, &str)] = &[
        ("check system status", "status"),
        ("show health report", "status"),
        ("what version is this", "version"),
        ("engine build revision", "version"),
        ("read the config file", "read_file"),
        ("show content of main.rs", "read_file"),
        ("list files in src", "list_directory"),
        ("ls the folder", "list_directory"),
        ("write notes to todo.md", "write_file"),
        ("save output file", "write_file"),
        ("fix the failing build", "self_heal_build"),
        ("repair compliance audit", "self_heal_build"),
    ];

    #[test]
    fn reflex_features_weigh_content_words_like_the_verb() {
        let f = SusiAlphaModel::reflex_features;
        // Under the fleet projection these were ~0.99 alike: "write" decided.
        assert!(
            cosine(
                &f("write a poem about the ocean"),
                &f("write notes to todo.md")
            ) < 0.6
        );
        // Stopwords and word order do not matter; synonyms still coincide.
        assert!(cosine(&f("check the system status"), &f("status system check")) > 0.999);
        assert!(
            cosine(&f("the"), &f("a")) == 0.0,
            "stopword-only prompts are empty"
        );
        let v = f("zorblat quux");
        assert!((v.iter().map(|x| x * x).sum::<f32>() - 1.0).abs() < 1e-5);
    }

    #[test]
    fn leading_category_word_no_longer_hijacks_unrelated_prompts() {
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join("staged.jsonl");
        stage(&staged, EVERYDAY);
        SusiAlphaModel::train_on_staged_file(dir.path(), &staged).unwrap();
        let model = SusiAlphaModel::load(dir.path()).unwrap();
        for prompt in [
            "write a poem about the ocean",
            "what is the capital of france",
            "tell me a joke",
            "delete everything in production",
        ] {
            assert!(
                model.predict_intent(prompt).is_err(),
                "{prompt} served a reflex"
            );
        }
        assert_eq!(
            model.predict_intent("list files in src").unwrap(),
            "ACTION: list_directory"
        );
    }

    #[test]
    fn anagrams_project_to_distinct_features() {
        let project = |w: &str| SusiAlphaModel::semantic_centroid_projection(w, None).unwrap();
        let words = ["stop", "post", "pots", "tops", "spot", "opts"];
        let vectors: Vec<Vec<f32>> = words.iter().map(|w| project(w)).collect();
        let distinct: std::collections::BTreeSet<usize> = vectors
            .iter()
            .map(|v| v.iter().position(|x| *x != 0.0).unwrap())
            .collect();
        assert!(distinct.len() >= 5, "anagrams share buckets: {distinct:?}");
        // Category anchors are unchanged: synonyms still coincide.
        assert_eq!(project("status"), project("health"));
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
    fn failed_mission_outcomes_are_never_trainable() {
        let content = [
            r#"{"intent":"a","action":"status","timestamp":1,"performance_metadata":{"outcome":"FAILED"}}"#,
            r#"{"intent":"b","action":"status","timestamp":2,"performance_metadata":{"outcome":"BLOCKED"}}"#,
            r#"{"intent":"c","action":"status","timestamp":3,"performance_metadata":{"outcome":"COMPLETE"}}"#,
            r#"{"intent":"d","action":"reason","timestamp":4,"performance_metadata":{"outcome":"SUCCESS"}}"#,
            // Receipt samples carry no outcome: a successful tool call.
            r#"{"intent":"e","action":"reason","timestamp":5,"performance_metadata":{"source":"tool_receipt"}}"#,
            r#"{"intent":"f","action":"reason","timestamp":6}"#,
        ]
        .join("\n");
        let parsed = parse_training_entries(&content, &intents()).unwrap();
        let kept: Vec<&str> = parsed
            .entries
            .iter()
            .map(|(entry, _)| entry.intent.as_str())
            .collect();
        assert_eq!(kept, ["c", "d", "e", "f"]);
        assert_eq!(parsed.skipped, 2);

        let only_failures = [
            r#"{"intent":"a","action":"status","timestamp":1,"performance_metadata":{"outcome":"FAILED"}}"#,
        ]
        .join("\n");
        assert!(parse_training_entries(&only_failures, &intents())
            .unwrap_err()
            .to_string()
            .contains("failed mission outcome"));
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
