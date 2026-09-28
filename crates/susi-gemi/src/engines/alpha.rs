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

/// An intent staged with more than one action in a batch keeps only the
/// strict-majority label; with no majority every sample of it is dropped.
/// Tool receipts stage `mission goal → tool` once per tool call, so one
/// multi-tool mission stages its goal under several labels, and without
/// this the replay merge's newest-wins rule would crown whichever tool ran
/// last. Returns the kept entries (batch order) and the dropped count.
fn resolve_label_conflicts(
    entries: Vec<(DistillationStaged, u32)>,
) -> (Vec<(DistillationStaged, u32)>, usize) {
    let mut votes: std::collections::HashMap<String, std::collections::HashMap<u32, usize>> =
        std::collections::HashMap::new();
    for (entry, label) in &entries {
        *votes
            .entry(entry.intent.trim().to_lowercase())
            .or_default()
            .entry(*label)
            .or_default() += 1;
    }
    let winner: std::collections::HashMap<String, Option<u32>> = votes
        .into_iter()
        .map(|(intent, tally)| {
            let total: usize = tally.values().sum();
            let majority = tally
                .into_iter()
                .find(|(_, count)| count * 2 > total)
                .map(|(label, _)| label);
            (intent, majority)
        })
        .collect();
    let before = entries.len();
    let kept: Vec<(DistillationStaged, u32)> = entries
        .into_iter()
        .filter(|(entry, label)| {
            winner
                .get(&entry.intent.trim().to_lowercase())
                .copied()
                .flatten()
                == Some(*label)
        })
        .collect();
    let dropped = before - kept.len();
    (kept, dropped)
}

/// Malformed or blank lines fail the batch. Well-formed records that cannot
/// be labeled (blank intent, failed mission outcome, action outside the
/// vocabulary, no majority label for the intent) are skipped and counted: a restored claim would otherwise
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
    let (entries, ambiguous) = resolve_label_conflicts(entries);
    skipped += ambiguous;
    // "No trainable distillation records" / "Empty distillation dataset" are
    // matched by `ReflexTrainer` (UNTRAINABLE_MARKERS) across the plane bus to
    // retire the claim instead of restoring it; keep them stable.
    if entries.is_empty() {
        if skipped > 0 {
            return Err(anyhow!(
                "No trainable distillation records: {skipped} skipped (blank intent, failed mission outcome, action outside the reflex vocabulary, or no majority label)."
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

/// Base intents every checkpoint keeps; never reclaimed.
const FOUNDATIONAL_INTENTS: [&str; 9] = [
    "status",
    "version",
    "self_heal_build",
    "run_test_harness",
    "write_file",
    "read_file",
    "list_directory",
    "scout",
    "reason",
];

/// Give staged actions that are real capabilities a vocabulary slot. The
/// vocabulary holds `DIM` actions and is first filled alphabetically, so
/// once it is full a newly installed — or merely late-alphabet — tool could
/// never become a reflex: its samples were skipped as "outside the
/// vocabulary" forever. A full vocabulary now reclaims a slot whose action
/// is not foundational and has no training support (absent from `supported`,
/// the replay set and the batch), i.e. was only ever trained on its
/// synthetic prime. Slots keep their index; the reused output row is refit
/// by the cycle's training. Returns the vocabulary and `(old, new)` pairs.
fn admit_staged_actions(
    mut vocabulary: Vec<String>,
    wanted: &[String],
    capabilities: &[String],
    supported: &std::collections::HashSet<String>,
) -> (Vec<String>, Vec<(String, String)>) {
    let lower = |s: &str| s.trim().to_lowercase();
    let mut reclaimed = Vec::new();
    for action in wanted {
        let key = lower(action);
        if vocabulary.iter().any(|known| lower(known) == key) {
            continue;
        }
        let Some(canonical) = capabilities.iter().find(|c| lower(c) == key) else {
            continue; // not a real capability: never earns a slot
        };
        if vocabulary.len() < SusiAlphaModel::DIM {
            vocabulary.push(canonical.clone());
            continue;
        }
        let victim = vocabulary.iter().rposition(|slot| {
            let slot_key = lower(slot);
            !FOUNDATIONAL_INTENTS.contains(&slot_key.as_str())
                && !supported.contains(&slot_key)
                && !wanted.iter().any(|w| lower(w) == slot_key)
        });
        if let Some(index) = victim {
            let old = std::mem::replace(&mut vocabulary[index], canonical.clone());
            reclaimed.push((old, canonical.clone()));
        }
    }
    (vocabulary, reclaimed)
}

/// Actions named by the staged lines, in first-seen order (unparseable
/// lines are ignored here; `parse_training_entries` rejects them).
fn staged_actions(content: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    content
        .lines()
        .filter_map(|line| serde_json::from_str::<DistillationStaged>(line).ok())
        .filter(|entry| !failed_outcome(entry))
        .map(|entry| entry.action.trim().to_string())
        .filter(|action| !action.is_empty() && seen.insert(action.to_lowercase()))
        .collect()
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
/// Full-batch steps stop once the mean loss reaches `TARGET_LOSS` (correct
/// class at ~0.86 probability) or at `MAX_EPOCHS`. A fixed 100 steps looked
/// fine on small batches but, measured on 962 samples over 20 actions, left
/// loss at 1.6: 95% argmax accuracy yet 0% of samples above the 0.5 serve
/// confidence — Tier-0 was right and silent. By 200 steps 92% served.
const TARGET_LOSS: f32 = 0.15;
const MAX_EPOCHS: usize = 600;

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

    /// Class-balanced fit: each sample's loss is weighted `n / (K · n_c)`
    /// so every class contributes equally however skewed the data. Plain
    /// mean NLL, measured with one action at 400 samples and nine at 6,
    /// reached the loss target by fitting the majority alone and served
    /// the *majority* action for every minority prompt at 0.55–0.83
    /// confidence. Returns the epochs run and the last measured loss.
    fn fit(&self, x: &Tensor, y: &Tensor, weights: &[f32]) -> Result<(usize, f32)> {
        let weights = Tensor::from_vec(weights.to_vec(), weights.len(), x.device())
            .map_err(crate::engines::candle_err::from_candle)?;
        let targets = y
            .unsqueeze(1)
            .map_err(crate::engines::candle_err::from_candle)?;
        let mut opt = AdamW::new(self.varmap.all_vars(), ParamsAdamW::default())
            .map_err(crate::engines::candle_err::from_candle)?;
        let mut last = f32::INFINITY;
        for epoch in 1..=MAX_EPOCHS {
            let log_sm = candle_nn::ops::log_softmax(&self.logits(x)?, 1)
                .map_err(crate::engines::candle_err::from_candle)?;
            // Weighted mean of -log p(label); weights sum to n.
            let picked = log_sm
                .gather(&targets, 1)
                .and_then(|t| t.squeeze(1))
                .map_err(crate::engines::candle_err::from_candle)?;
            let loss = (picked * &weights)
                .and_then(|t| t.mean_all())
                .and_then(|t| t.neg())
                .map_err(crate::engines::candle_err::from_candle)?;
            last = loss
                .to_scalar::<f32>()
                .map_err(crate::engines::candle_err::from_candle)?;
            if last <= TARGET_LOSS {
                return Ok((epoch - 1, last));
            }
            opt.backward_step(&loss)
                .map_err(crate::engines::candle_err::from_candle)?;
        }
        Ok((MAX_EPOCHS, last))
    }

    /// Accuracy (argmax over the first `vocabulary` outputs equals the
    /// label; a label the vocabulary cannot express is a miss) plus the error that serving actually risks: a wrong argmax
    /// whose probability (softmax over every output, as `predict_intent`
    /// computes it) clears `SERVE_CONFIDENCE` would be served.
    fn evaluate(&self, x: &Tensor, labels: &[u32], vocabulary: usize) -> Result<Evaluation> {
        let rows = candle_nn::ops::softmax(&self.logits(x)?, 1)
            .and_then(|p| p.to_vec2::<f32>())
            .map_err(crate::engines::candle_err::from_candle)?;
        let mut eval = Evaluation::default();
        for (row, &label) in rows.iter().zip(labels) {
            let Some((index, p)) = row
                .iter()
                .take(vocabulary)
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
            else {
                continue;
            };
            if Some(index) == usize::try_from(label).ok() {
                eval.correct += 1;
            } else if *p > SERVE_CONFIDENCE {
                eval.wrong_served += 1;
            }
        }
        Ok(eval)
    }
}

/// Held-out scoring of one model.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Evaluation {
    correct: usize,
    /// Wrong predictions confident enough that Tier-0 would serve them.
    wrong_served: usize,
}

/// `predict_intent` serves a reflex only above this probability; the
/// held-out gate counts errors above it as served mistakes.
const SERVE_CONFIDENCE: f32 = 0.5;

/// A staged or primed training sample: intent text and its action label.
type Labeled<'a> = (&'a str, u32);

/// Weights for a fit over `staged` then `primes` (in that order): staged
/// samples are class-balanced; each synthetic prime weighs 1. A prime is a
/// seed so an unused action stays reachable, not evidence — balanced as a
/// class of one it would outweigh every real sample of a contradicting
/// label that shares its features.
fn fit_weights(staged: &[(&str, u32)], primes: usize) -> Vec<f32> {
    let labels: Vec<u32> = staged.iter().map(|(_, label)| *label).collect();
    let mut weights = class_balance_weights(&labels);
    weights.extend(std::iter::repeat_n(1.0, primes));
    weights
}

/// Per-sample weights `n / (K · n_c)` (K = distinct labels): each class's
/// weights sum to `n / K`, and all weights sum to `n`.
fn class_balance_weights(labels: &[u32]) -> Vec<f32> {
    let mut counts: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
    for label in labels {
        *counts.entry(*label).or_default() += 1;
    }
    let n = labels.len() as f32;
    let k = counts.len().max(1) as f32;
    labels
        .iter()
        .map(|label| n / (k * counts.get(label).copied().unwrap_or(1) as f32))
        .collect()
}

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
    /// Reflex features of every replayed training intent (flattened
    /// `n × DIM`) and each row's action. `None` when the checkpoint has no
    /// readable replay set (bootstrap or pre-replay checkpoints), which keeps
    /// the confidence-only legacy gate.
    support: Option<SupportSet>,
}

struct SupportSet {
    features: Vec<f32>,
    actions: Vec<String>,
    /// Lowercased words each action was trained with (replay intents plus
    /// its own name), for the veto-word guard.
    action_words: std::collections::HashMap<String, std::collections::HashSet<String>>,
}

/// Words that negate or reverse a request, or name a destructive/control
/// operation. The reflex features are a bag of words, so they cannot see
/// that "don't read the file" is not "read the file" — measured on the
/// Tier-0 benchmark model: "delete the config file" was served
/// `write_file`, "remove all files" `list_directory`, "don't read the
/// file" `read_file`, "do not run the tests" `run_test_harness`, "shutdown
/// the system" `status`, "stop the build" `version`. A prompt containing
/// one of these is served only if the predicted action was trained with
/// that very word (a reflex truly taught "delete temp files" still works).
const VETO_WORDS: &[&str] = &[
    // negation / reversal ("don't" splits to "don" + "t")
    "not",
    "no",
    "never",
    "don",
    "dont",
    "doesn",
    "didn",
    "isn",
    "without",
    "except",
    "stop",
    "cancel",
    "avoid",
    "skip",
    "undo",
    "revert",
    "rollback",
    // destructive / control operations
    "delete",
    "remove",
    "rm",
    "erase",
    "wipe",
    "drop",
    "destroy",
    "purge",
    "truncate",
    "kill",
    "shutdown",
    "reboot",
    "uninstall",
    "format",
    "reset",
    "overwrite",
];

fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
}

/// Below `SERVE_CONFIDENCE`, a prediction is still served when the prompt is
/// a near-duplicate (cosine ≥ `NEIGHBOR_AGREEMENT`) of a trained intent
/// whose action is the one predicted, and confidence clears
/// `AGREED_CONFIDENCE`: two independent signals — the classifier and the
/// nearest trained example — agree. Motivated by the Tier-0 benchmark's only
/// miss, "show version": support 0.95, confidence 0.46–0.48, refused.
const NEIGHBOR_AGREEMENT: f32 = 0.9;
const AGREED_CONFIDENCE: f32 = 0.35;

/// The confidence half of the serve decision (support is checked first):
/// confident, or near-duplicate agreement with a trained example.
fn serves(action: &str, confidence: f32, nearest: Option<(f32, &str)>) -> bool {
    if confidence > SERVE_CONFIDENCE {
        return true;
    }
    confidence > AGREED_CONFIDENCE
        && nearest.is_some_and(|(cosine, neighbor)| {
            cosine >= NEIGHBOR_AGREEMENT
                && action
                    .strip_prefix("ACTION: ")
                    .is_some_and(|predicted| predicted.eq_ignore_ascii_case(neighbor))
        })
}

/// A prompt is served a Tier-0 reflex only when its features sit at least
/// this close (cosine) to some intent the model was trained on. Everyday
/// out-of-distribution prompts measured ≤ 0.52 against everyday training
/// data; a paraphrase sharing two of three content words is ~0.67.
const SUPPORT_MIN: f32 = 0.6;

/// The replay set's features, or `None` (confidence-only gate) when there
/// is no replay set *or it cannot be read*: the support set is a serving
/// refinement, and an unreadable file must degrade Tier-0, never disable
/// it — as an error here would fail the whole checkpoint load.
fn support_matrix(weights_path: &Path, vocabulary: &[String]) -> Option<SupportSet> {
    let replay = match load_replay(weights_path) {
        Ok(replay) => replay,
        Err(error) => {
            // Construction records the degradation in the error-metrics sink.
            let _emit = crate::susi_error::EaiError::io(format!(
                "reflex support set unavailable, serving on confidence only: {error}"
            ));
            return None;
        }
    };
    // Every vocabulary action is also trained on its own name (the synthetic
    // prime), so the bare name is supported too: "status" or "scout" alone
    // was refused as unfamiliar unless that exact wording had been staged.
    (!replay.is_empty()).then(|| {
        let rows: Vec<(&str, &str)> = replay
            .iter()
            .map(|sample| (sample.intent.as_str(), sample.action.as_str()))
            .chain(
                vocabulary
                    .iter()
                    .map(|action| (action.as_str(), action.as_str())),
            )
            .collect();
        let mut action_words: std::collections::HashMap<String, std::collections::HashSet<String>> =
            std::collections::HashMap::new();
        for (intent, action) in &rows {
            action_words
                .entry(action.to_lowercase())
                .or_default()
                .extend(words(intent));
        }
        SupportSet {
            features: rows
                .iter()
                .flat_map(|(intent, _)| SusiAlphaModel::reflex_features(intent))
                .collect(),
            actions: rows
                .iter()
                .map(|(_, action)| (*action).to_string())
                .collect(),
            action_words,
        }
    })
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
                    let support = support_matrix(&weights_path, &intents);
                    Ok(Self {
                        fc1,
                        fc2,
                        intents,
                        support,
                    })
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

        Ok(Self {
            fc1,
            fc2,
            intents,
            support: None,
        })
    }

    /// Dynamic Intent Surface Discovery
    pub fn list_dynamic_intents() -> Vec<String> {
        let mut intents: Vec<String> = FOUNDATIONAL_INTENTS.map(String::from).to_vec();
        // Dynamic entries fill remaining DIM capacity after the foundational
        // intents, so truncation can never evict a base intent when the
        // registry is crowded. Truncation is alphabetical; slots for the
        // capabilities actually used are won back by `admit_staged_actions`.
        let mut dynamic = Self::capability_names();
        dynamic.truncate(Self::DIM.saturating_sub(intents.len()));
        intents.extend(dynamic);
        intents.sort();
        intents
    }

    /// Every registered agent and installed tool name (sorted, deduplicated,
    /// foundational intents excluded) — untruncated, unlike the vocabulary.
    pub fn capability_names() -> Vec<String> {
        let mut names: Vec<String> = crate::susi_core::AgentMetaRegistry::global()
            .list_agents()
            .into_iter()
            .map(|agent| agent.name)
            .chain(crate::susi_core::registry::CapabilityRegistry::global().list_tools())
            .filter(|name| !FOUNDATIONAL_INTENTS.contains(&name.as_str()))
            .collect();
        names.sort();
        names.dedup();
        names
    }

    pub fn train_on_staged_data(global_dir: &Path) -> Result<String> {
        Self::train_on_staged_file(global_dir, &global_dir.join("distillation_staged.jsonl"))
    }

    pub fn train_on_staged_file(global_dir: &Path, staged_file: &Path) -> Result<String> {
        let training_lock =
            crate::susi_config::file_lock::FileLock::acquire(global_dir, "reflex_training")
                .ok_or_else(|| anyhow!("reflex training lock unavailable"))?;
        // A gated cycle is two fits of up to MAX_EPOCHS each — measured ~53s
        // at 2048 samples without convergence, close to the lock's 60s
        // wedged-holder age. Keep it fresh so a concurrent trainer cannot
        // break it and publish over this cycle.
        training_lock.hold_while(|| Self::train_locked(global_dir, staged_file))
    }

    fn train_locked(global_dir: &Path, staged_file: &Path) -> Result<String> {
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
        let prior = load_replay(&weights_path)?;
        let supported: std::collections::HashSet<String> = prior
            .iter()
            .map(|sample| sample.action.trim().to_lowercase())
            .collect();
        let (dynamic_intents, reclaimed) = admit_staged_actions(
            dynamic_intents,
            &staged_actions(&content),
            &Self::capability_names(),
            &supported,
        );
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
        let (epochs, loss) = net.fit(&x, &y, &fit_weights(&staged, primes.len()))?;
        let samples = all.len();

        // Before publication: the published bundle's mtime is the model
        // cache key, and a loaded model's support set is read from the
        // replay file, so the file must already hold this cycle's samples.
        // A failed write costs only this cycle's replay additions.
        let replay_note = match save_replay(&weights_path, &replayed) {
            Ok(()) => format!(" Replayed {replay_count} prior sample(s)."),
            Err(error) => format!(" Replayed {replay_count} prior sample(s); {error}."),
        };
        let publication = publish_checkpoint(&net.varmap, &weights_path, &dynamic_intents)?;
        let reclaimed_note = if reclaimed.is_empty() {
            String::new()
        } else {
            let pairs: Vec<String> = reclaimed
                .iter()
                .map(|(old, new)| format!("{old} -> {new}"))
                .collect();
            format!(" Reclaimed vocabulary slot(s): {}.", pairs.join(", "))
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
                " Skipped {} untrainable record(s) (blank intent, failed mission outcome, action outside the reflex vocabulary, or no majority label).",
                batch.skipped
            )
        };
        Ok(format!("Native distillation complete. Trained cumulatively on {samples} samples with the dynamic intent surface ({epochs} epochs, loss {loss:.3}).{replay_note}{reclaimed_note}{gate}{skipped}{cleanup}"))
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
        let baseline = ReflexNet::init(Some(checkpoint), device)?.evaluate(
            &hx,
            &held_labels,
            active_vocabulary,
        )?;
        let candidate = ReflexNet::init(Some(checkpoint), device)?;
        let fit: Vec<(&str, u32)> = train.iter().chain(primes).copied().collect();
        let (x, y) = labeled_batch(&fit, device)?;
        let _ = candidate.fit(&x, &y, &fit_weights(&train, primes.len()))?;
        let scored = candidate.evaluate(&hx, &held_labels, vocabulary.len())?;
        let n = holdout.len();
        let summary = format!(
            "correct {}/{n}, wrong-but-served {} (active: correct {}/{n}, wrong-but-served {})",
            scored.correct, scored.wrong_served, baseline.correct, baseline.wrong_served
        );
        // Accuracy must not drop, and neither may the mistakes Tier-0 would
        // actually serve rise: equal accuracy with more confident errors is
        // a worse reflex.
        if scored.correct < baseline.correct || scored.wrong_served > baseline.wrong_served {
            // "reflex checkpoint held back" is matched by `ReflexTrainer`
            // (HELD_BACK_MARKER) across the plane bus to log the cycle and
            // back off; keep the prefix stable.
            return Err(anyhow!(
                "reflex checkpoint held back: held-out {summary}; active checkpoint kept"
            ));
        }
        Ok(format!(" Held-out gate passed: {summary}."))
    }

    /// Operator view of the published Tier-0 model: active checkpoint,
    /// vocabulary fill (reclamation starts at `DIM`), and replay-set size.
    /// Read-only; never publishes a bootstrap model the way `load` does.
    pub fn inventory(global_dir: &Path) -> serde_json::Value {
        let Ok(config) = crate::susi_sandbox::manager::SusiConfig::load(global_dir) else {
            return serde_json::json!({ "error": "config unreadable" });
        };
        let weights_path = global_dir
            .join("models")
            .join(config.alpha_weights_filename());
        let checkpoint = match usable_checkpoint(&weights_path) {
            Ok(checkpoint) => checkpoint,
            Err(error) => return serde_json::json!({ "error": error.to_string() }),
        };
        let replay = load_replay(&weights_path).unwrap_or_default();
        let replay_actions: std::collections::BTreeSet<String> =
            replay.iter().map(|s| s.action.to_lowercase()).collect();
        serde_json::json!({
            "checkpoint": checkpoint.as_ref().and_then(|(path, _)| {
                path.file_name().map(|n| n.to_string_lossy().to_string())
            }),
            "vocabulary": checkpoint.as_ref().map_or(0, |(_, intents)| intents.len()),
            "vocabulary_capacity": Self::DIM,
            "replay_samples": replay.len(),
            "replay_actions": replay_actions.len(),
        })
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
        let nearest = self.nearest(prompt);
        if let Some((support, _)) = nearest {
            if support < SUPPORT_MIN {
                return Err(anyhow!(
                    "Unfamiliar prompt: nearest trained intent at cosine {support:.2} (< {SUPPORT_MIN})."
                ));
            }
        }
        let (action, confidence) = self.predict_intent_with_confidence(prompt)?;
        if let Some(word) = self.vetoed_word(prompt, &action) {
            return Err(anyhow!(
                "Vetoed: '{word}' negates or reverses the request, and Tier-0 never learned it with {action}."
            ));
        }
        if serves(&action, confidence, nearest) {
            return Ok(action);
        }
        Err(anyhow!(
            "Low confidence ({:.2}) in neural reflex.",
            confidence
        ))
    }

    /// Cosine between `prompt` and the nearest intent in the replay set, or
    /// `None` when this checkpoint carries no replay set. Features are unit
    /// vectors, so the dot product is the cosine.
    pub fn support(&self, prompt: &str) -> Option<f32> {
        self.nearest(prompt).map(|(cosine, _)| cosine)
    }

    /// The first veto word in `prompt` that the predicted action was never
    /// trained with. `None` without a support set (legacy checkpoints keep
    /// the confidence-only gate) or when every veto word was learned.
    fn vetoed_word(&self, prompt: &str, action: &str) -> Option<String> {
        let set = self.support.as_ref()?;
        let predicted = action
            .strip_prefix("ACTION: ")
            .unwrap_or(action)
            .to_lowercase();
        let learned = set.action_words.get(&predicted);
        words(prompt).find(|w| {
            VETO_WORDS.contains(&w.as_str()) && !learned.is_some_and(|known| known.contains(w))
        })
    }

    /// The nearest replayed intent: its cosine and its action.
    fn nearest(&self, prompt: &str) -> Option<(f32, &str)> {
        let set = self.support.as_ref()?;
        let query = Self::reflex_features(prompt);
        set.features
            .as_chunks::<{ Self::DIM }>()
            .0
            .iter()
            .map(|row| row.iter().zip(&query).map(|(a, b)| a * b).sum::<f32>())
            .zip(&set.actions)
            .max_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(cosine, action)| (cosine, action.as_str()))
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
    fn fit_trains_to_serving_confidence_at_realistic_scale() {
        let device = susi_vendor_candle::candle_core::Device::Cpu;
        let verbs = [
            "deploy",
            "restart",
            "backup",
            "migrate",
            "compile",
            "lint",
            "benchmark",
            "profile",
            "translate",
            "summarize",
            "index",
            "archive",
            "encrypt",
            "rotate",
            "scale",
            "monitor",
            "trace",
            "vacuum",
            "snapshot",
            "rollback",
        ];
        let nouns = [
            "api", "database", "cluster", "cache", "frontend", "worker", "queue", "gateway",
            "logs", "metrics", "keys", "service", "bucket", "schema", "pipeline",
        ];
        let texts: Vec<(String, u32)> = verbs
            .iter()
            .zip(0u32..)
            .flat_map(|(verb, label)| {
                nouns.iter().flat_map(move |noun| {
                    ["now", "today", "safely"].map(|m| (format!("{verb} the {noun} {m}"), label))
                })
            })
            .collect();
        let samples: Vec<(&str, u32)> = texts.iter().map(|(t, l)| (t.as_str(), *l)).collect();
        let labels: Vec<u32> = samples.iter().map(|(_, l)| *l).collect();
        let (x, y) = labeled_batch(&samples, &device).unwrap();
        let net = ReflexNet::init(None, &device).unwrap();
        let (epochs, loss) = net.fit(&x, &y, &class_balance_weights(&labels)).unwrap();
        assert!(epochs > 100, "900 samples cannot converge in 100 steps");
        assert!(loss <= TARGET_LOSS || epochs == MAX_EPOCHS);
        let probs = candle_nn::ops::softmax(&net.logits(&x).unwrap(), 1)
            .unwrap()
            .to_vec2::<f32>()
            .unwrap();
        let served = probs
            .iter()
            .zip(&labels)
            .filter(|(row, &label)| {
                let (index, p) = row
                    .iter()
                    .take(verbs.len())
                    .enumerate()
                    .max_by(|a, b| a.1.total_cmp(b.1))
                    .unwrap();
                index as u32 == label && *p > 0.5
            })
            .count();
        assert!(
            served * 10 >= samples.len() * 8,
            "only {served}/{} correct and above the serve bar",
            samples.len()
        );
    }

    #[test]
    fn class_balance_weights_equalize_class_mass() {
        let w = class_balance_weights(&[0, 0, 0, 1]);
        assert_eq!(w, [4.0 / 6.0, 4.0 / 6.0, 4.0 / 6.0, 2.0]);
        assert!((w.iter().sum::<f32>() - 4.0).abs() < 1e-5);
    }

    #[test]
    fn a_majority_action_does_not_swallow_minority_actions() {
        let device = susi_vendor_candle::candle_core::Device::Cpu;
        let verbs = [
            "deploy",
            "restart",
            "backup",
            "migrate",
            "compile",
            "lint",
            "benchmark",
            "profile",
            "translate",
            "summarize",
        ];
        let nouns = [
            "api",
            "database",
            "cluster",
            "cache",
            "frontend",
            "worker",
            "queue",
            "gateway",
            "logs",
            "metrics",
            "keys",
            "service",
            "bucket",
            "schema",
            "pipeline",
            "ledger",
            "mailer",
            "scheduler",
            "indexer",
            "router",
        ];
        let mods = [
            "now",
            "today",
            "safely",
            "quickly",
            "again",
            "tonight",
            "carefully",
            "first",
            "later",
            "soon",
        ];
        // Label 0 has 400 samples; labels 1..9 have 6 each (all "<verb> the
        // api <mod>"), so a minority verb has never been seen with nouns 10+.
        let mut texts: Vec<(String, u32)> = Vec::new();
        for (verb, label) in verbs.iter().zip(0u32..) {
            let per = if label == 0 { 400 } else { 6 };
            let phrases = nouns
                .iter()
                .flat_map(|noun| mods.iter().map(move |m| format!("{verb} the {noun} {m}")));
            texts.extend(phrases.take(per).map(|t| (t, label)));
        }
        let train: Vec<(&str, u32)> = texts.iter().map(|(t, l)| (t.as_str(), *l)).collect();
        let labels: Vec<u32> = train.iter().map(|(_, l)| *l).collect();
        let (x, y) = labeled_batch(&train, &device).unwrap();
        let net = ReflexNet::init(None, &device).unwrap();
        net.fit(&x, &y, &class_balance_weights(&labels)).unwrap();

        // Unseen combinations of seen words for each minority verb.
        let probes: Vec<(String, u32)> = verbs[1..]
            .iter()
            .zip(1u32..)
            .flat_map(|(verb, label)| {
                nouns[10..14]
                    .iter()
                    .map(move |noun| (format!("{verb} the {noun} now"), label))
            })
            .collect();
        let texts: Vec<&str> = probes.iter().map(|(t, _)| t.as_str()).collect();
        let probs = candle_nn::ops::softmax(
            &net.logits(&projection_batch(&texts, &device).unwrap())
                .unwrap(),
            1,
        )
        .unwrap()
        .to_vec2::<f32>()
        .unwrap();
        let (mut correct, mut wrong_and_served) = (0, 0);
        for (row, (_, label)) in probs.iter().zip(&probes) {
            let (index, p) = row
                .iter()
                .take(verbs.len())
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .unwrap();
            if index as u32 == *label {
                correct += 1;
            } else if *p > 0.5 {
                wrong_and_served += 1;
            }
        }
        // Measured over 5 inits each: uniform loss 1-11/36 correct with
        // 13-28 wrong-but-served; balanced 34-36/36 with 0-2.
        assert!(correct >= 30, "minority correct {correct}/36");
        assert!(
            wrong_and_served <= 4,
            "wrong-but-served {wrong_and_served}/36"
        );
    }

    const BENCH_TRAIN: &[(&str, &str)] = &[
        ("check system status", "status"),
        ("show system health", "status"),
        ("what is the current state", "status"),
        ("hardware status report", "status"),
        ("health check please", "status"),
        ("report system state", "status"),
        ("is the system healthy", "status"),
        ("status of the daemon", "status"),
        ("check health", "status"),
        ("give me a status report", "status"),
        ("what version is running", "version"),
        ("show the build version", "version"),
        ("engine revision", "version"),
        ("which version of susi", "version"),
        ("print version", "version"),
        ("current build number", "version"),
        ("version info", "version"),
        ("what revision is deployed", "version"),
        ("show engine version", "version"),
        ("build revision info", "version"),
        ("fix the broken build", "self_heal_build"),
        ("repair the build", "self_heal_build"),
        ("heal compilation errors", "self_heal_build"),
        ("fix compile failures", "self_heal_build"),
        ("the build is broken fix it", "self_heal_build"),
        ("repair failing build", "self_heal_build"),
        ("auto heal build errors", "self_heal_build"),
        ("fix build breakage", "self_heal_build"),
        ("heal the build", "self_heal_build"),
        ("repair compile errors", "self_heal_build"),
        ("run the tests", "run_test_harness"),
        ("execute test suite", "run_test_harness"),
        ("run unit tests", "run_test_harness"),
        ("launch the test harness", "run_test_harness"),
        ("run all tests", "run_test_harness"),
        ("execute the tests now", "run_test_harness"),
        ("test everything", "run_test_harness"),
        ("run integration tests", "run_test_harness"),
        ("kick off the test run", "run_test_harness"),
        ("run tests again", "run_test_harness"),
        ("write notes to todo.md", "write_file"),
        ("save this to a file", "write_file"),
        ("create file config.toml", "write_file"),
        ("put this text in notes.txt", "write_file"),
        ("update the readme file", "write_file"),
        ("save output to report.md", "write_file"),
        ("create a new file", "write_file"),
        ("write the summary to disk", "write_file"),
        ("save the draft", "write_file"),
        ("write file hello.txt", "write_file"),
        ("read the config file", "read_file"),
        ("show contents of main.rs", "read_file"),
        ("cat the readme", "read_file"),
        ("get file contents", "read_file"),
        ("read notes.txt", "read_file"),
        ("show me cargo.toml", "read_file"),
        ("fetch the log content", "read_file"),
        ("read the file", "read_file"),
        ("show file content", "read_file"),
        ("open and read todo.md", "read_file"),
        ("list files in src", "list_directory"),
        ("ls the folder", "list_directory"),
        ("list the directory", "list_directory"),
        ("show files here", "list_directory"),
        ("what files are in this folder", "list_directory"),
        ("list directory contents", "list_directory"),
        ("ls", "list_directory"),
        ("dir listing", "list_directory"),
        ("list all files", "list_directory"),
        ("show folder contents", "list_directory"),
        ("scout for mcp servers", "scout"),
        ("search for new tools", "scout"),
        ("discover mcp tools", "scout"),
        ("find available servers", "scout"),
        ("look for plugins", "scout"),
        ("scout the network", "scout"),
        ("discover new capabilities", "scout"),
        ("search mcp registry", "scout"),
        ("find tools online", "scout"),
        ("scout for agents", "scout"),
        ("think about this problem", "reason"),
        ("solve this complex task", "reason"),
        ("reason through the design", "reason"),
        ("calculate the total", "reason"),
        ("think step by step", "reason"),
        ("solve the equation", "reason"),
        ("reason about tradeoffs", "reason"),
        ("think it through", "reason"),
        ("solve this puzzle", "reason"),
        ("calculate the cost", "reason"),
    ];
    const BENCH_TEST: &[(&str, &str)] = &[
        ("check the system health", "status"),
        ("status report please", "status"),
        ("system state check", "status"),
        ("show version", "version"),
        ("which build revision", "version"),
        ("engine version info", "version"),
        ("fix the build errors", "self_heal_build"),
        ("repair the broken compile", "self_heal_build"),
        ("heal build", "self_heal_build"),
        ("run the test suite", "run_test_harness"),
        ("execute all tests", "run_test_harness"),
        ("run tests", "run_test_harness"),
        ("write to notes.md", "write_file"),
        ("save report file", "write_file"),
        ("create config file", "write_file"),
        ("read main.rs", "read_file"),
        ("show me the file content", "read_file"),
        ("cat config", "read_file"),
        ("list files", "list_directory"),
        ("ls src", "list_directory"),
        ("show directory files", "list_directory"),
        ("discover mcp servers", "scout"),
        ("search for tools", "scout"),
        ("scout for plugins", "scout"),
        ("think about the problem", "reason"),
        ("solve this", "reason"),
        ("calculate the sum", "reason"),
    ];
    const BENCH_OOD: &[&str] = &[
        "what is the capital of france",
        "write a poem about the ocean",
        "translate hello into german",
        "who won the world cup in 2018",
        "explain quantum entanglement simply",
        "hi",
        "tell me a joke",
        "delete everything in production",
        "book a flight to tokyo",
        "what's the weather tomorrow",
        "summarize this article",
        "how do i bake bread",
        "recommend a good movie",
        "convert 5 miles to km",
        "draft an email to my boss",
        "what is love",
        "play some music",
        "order a pizza",
        "compose a haiku about autumn",
        "set a reminder for 5pm",
    ];

    #[test]
    fn bare_action_names_are_supported_by_their_primes() {
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join("staged.jsonl");
        stage(&staged, EVERYDAY);
        SusiAlphaModel::train_on_staged_file(dir.path(), &staged).unwrap();
        let model = SusiAlphaModel::load(dir.path()).unwrap();
        // EVERYDAY never stages "scout" or "reason": only their primes train.
        for (name, action) in [("scout", "ACTION: scout"), ("reason", "ACTION: reason")] {
            assert!(model.support(name).unwrap() > 0.99, "{name}");
            assert_eq!(model.predict_intent(name).unwrap(), action);
        }
        assert!(model.predict_intent("tell me a joke").is_err());
    }

    #[test]
    fn negated_and_destructive_prompts_are_vetoed_unless_learned() {
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join("staged.jsonl");
        // The benchmark corpus plus one action genuinely taught a veto word.
        let mut corpus = BENCH_TRAIN.to_vec();
        corpus.extend([
            ("delete the build cache", "self_heal_build"),
            ("delete stale build artifacts", "self_heal_build"),
        ]);
        stage(&staged, &corpus);
        SusiAlphaModel::train_on_staged_file(dir.path(), &staged).unwrap();
        let model = SusiAlphaModel::load(dir.path()).unwrap();
        // Each was served as the wrong action before the guard (measured).
        for prompt in [
            "delete the config file",
            "remove all files",
            "don't read the file",
            "never list files",
            "do not run the tests",
            "shutdown the system",
            "stop the build",
        ] {
            assert!(model.predict_intent(prompt).is_err(), "{prompt} was served");
        }
        // A veto word the predicted action was trained with does not block it.
        assert_eq!(
            model.vetoed_word("delete the build cache", "ACTION: self_heal_build"),
            None
        );
        assert_eq!(
            model.vetoed_word("delete the config file", "ACTION: write_file"),
            Some("delete".to_string())
        );
    }

    #[test]
    fn low_confidence_serves_only_with_an_agreeing_near_duplicate() {
        let a = "ACTION: version";
        assert!(serves(a, 0.51, None), "confident enough on its own");
        assert!(!serves(a, 0.48, None), "no support set: confidence only");
        assert!(serves(a, 0.48, Some((0.95, "version"))));
        assert!(serves(a, 0.48, Some((0.95, "VERSION"))));
        assert!(
            !serves(a, 0.48, Some((0.95, "status"))),
            "neighbor disagrees"
        );
        assert!(
            !serves(a, 0.48, Some((0.85, "version"))),
            "not a near-duplicate"
        );
        assert!(
            !serves(a, 0.30, Some((0.99, "version"))),
            "too unsure even so"
        );
    }

    /// Tier-0 quality benchmark: a fixed corpus of ~10 phrasings per
    /// foundational intent, unseen paraphrases, and everyday prompts that
    /// are not commands, run through the production publish and
    /// `predict_intent` path (confidence + support gates). Baseline when
    /// written (3 inits): recall 26-27/27, served precision 100%, 0/20
    /// out-of-distribution prompts served; with neighbor agreement
    /// (EV-CLAUDE-023) 27/27 in 5 of 5 inits. Bounds leave room for init noise
    /// but fail on any real regression in features, loss or gating.
    #[test]
    fn tier0_benchmark_recall_precision_and_ood_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join("staged.jsonl");
        stage(&staged, BENCH_TRAIN);
        SusiAlphaModel::train_on_staged_file(dir.path(), &staged).unwrap();
        let model = SusiAlphaModel::load(dir.path()).unwrap();

        let (mut served, mut correct) = (0, 0);
        for (prompt, action) in BENCH_TEST {
            if let Ok(served_action) = model.predict_intent(prompt) {
                served += 1;
                if served_action == format!("ACTION: {action}") {
                    correct += 1;
                }
            }
        }
        let ood_served: Vec<&str> = BENCH_OOD
            .iter()
            .copied()
            .filter(|prompt| model.predict_intent(prompt).is_ok())
            .collect();
        assert!(correct >= 25, "recall {correct}/{}", BENCH_TEST.len());
        assert!(
            served - correct <= 1,
            "wrong served {}/{served}",
            served - correct
        );
        assert!(
            ood_served.len() <= 1,
            "out-of-distribution served: {ood_served:?}"
        );
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
    fn inventory_reports_checkpoint_vocabulary_and_replay() {
        let dir = tempfile::tempdir().unwrap();
        let empty = SusiAlphaModel::inventory(dir.path());
        assert_eq!(empty["checkpoint"], serde_json::Value::Null);
        assert_eq!(empty["replay_samples"], 0);
        assert!(
            !dir.path().join("models").exists(),
            "inventory must not publish a bootstrap model"
        );

        let staged = dir.path().join("staged.jsonl");
        stage(&staged, EVERYDAY);
        SusiAlphaModel::train_on_staged_file(dir.path(), &staged).unwrap();
        let inv = SusiAlphaModel::inventory(dir.path());
        assert!(inv["checkpoint"]
            .as_str()
            .unwrap()
            .ends_with(".safetensors"));
        assert!(inv["vocabulary"].as_u64().unwrap() >= 9);
        assert_eq!(inv["vocabulary_capacity"], SusiAlphaModel::DIM);
        assert_eq!(inv["replay_samples"], EVERYDAY.len());
        assert_eq!(inv["replay_actions"], 6);
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
        assert!(
            // ReflexTrainer's HELD_BACK_MARKER; Display adds a category prefix.
            error.to_string().contains("reflex checkpoint held back"),
            "{error}"
        );
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
    fn evaluation_counts_confident_mistakes_as_served() {
        let device = susi_vendor_candle::candle_core::Device::Cpu;
        let phrases = status_phrases();
        let train: Vec<(&str, u32)> = phrases.iter().take(20).map(|p| (p.as_str(), 1)).collect();
        let (x, y) = labeled_batch(&train, &device).unwrap();
        let net = ReflexNet::init(None, &device).unwrap();
        net.fit(&x, &y, &vec![1.0; train.len()]).unwrap();
        let probe = projection_batch(&["status check"], &device).unwrap();
        // Confidently predicts label 1.
        assert_eq!(
            net.evaluate(&probe, &[1], 4).unwrap(),
            Evaluation {
                correct: 1,
                wrong_served: 0
            }
        );
        // The same confident prediction against label 0 is a served mistake.
        assert_eq!(
            net.evaluate(&probe, &[0], 4).unwrap(),
            Evaluation {
                correct: 0,
                wrong_served: 1
            }
        );
        // A vocabulary that cannot express label 1 argmaxes elsewhere with
        // low probability: a miss, but not one Tier-0 would serve.
        assert_eq!(
            net.evaluate(&probe, &[0], 1).unwrap(),
            Evaluation {
                correct: 1,
                wrong_served: 0
            }
        );
    }

    #[test]
    fn prediction_scoring_counts_only_the_expressible_vocabulary() {
        let device = susi_vendor_candle::candle_core::Device::Cpu;
        let net = ReflexNet::init(None, &device).unwrap();
        let x = projection_batch(&["status check"], &device).unwrap();
        // Whatever the random net predicts, an empty vocabulary predicts
        // nothing and a label beyond it can never count as correct.
        assert_eq!(net.evaluate(&x, &[0], 0).unwrap().correct, 0);
        let all = net.evaluate(&x, &[0], SusiAlphaModel::DIM).unwrap().correct
            + (1..SusiAlphaModel::DIM as u32)
                .map(|label| {
                    net.evaluate(&x, &[label], SusiAlphaModel::DIM)
                        .unwrap()
                        .correct
                })
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
    fn unfamiliar_prompts_are_refused_even_when_confident() {
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join("staged.jsonl");
        stage(&staged, EVERYDAY);
        SusiAlphaModel::train_on_staged_file(dir.path(), &staged).unwrap();
        let model = SusiAlphaModel::load(dir.path()).unwrap();

        let familiar = model.support("check the system status please").unwrap();
        assert!(familiar > 0.9, "{familiar}");
        let unfamiliar = model.support("write a poem about the ocean").unwrap();
        assert!(unfamiliar < SUPPORT_MIN, "{unfamiliar}");
        let error = model
            .predict_intent("write a poem about the ocean")
            .unwrap_err();
        assert!(error.to_string().contains("Unfamiliar prompt"), "{error}");

        // No replay set (bootstrap / pre-replay checkpoint): legacy gate.
        let fresh = tempfile::tempdir().unwrap();
        assert_eq!(
            SusiAlphaModel::load(fresh.path()).unwrap().support("x"),
            None
        );

        // An unreadable replay set degrades to the legacy gate; the trained
        // checkpoint still loads and serves.
        let filename = crate::susi_sandbox::manager::SusiConfig::load(dir.path())
            .unwrap()
            .alpha_weights_filename();
        let replay = replay_path(&dir.path().join("models").join(filename));
        std::fs::remove_file(&replay).unwrap();
        std::fs::create_dir(&replay).unwrap(); // reading a directory fails
        let degraded = SusiAlphaModel::load(dir.path()).unwrap();
        assert_eq!(degraded.support("list files in src"), None);
        assert_eq!(
            degraded.predict_intent("list files in src").unwrap(),
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
    fn full_vocabulary_reclaims_unsupported_slots_for_used_capabilities() {
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<String>>();
        // A full vocabulary: foundational intents plus alphabetical filler.
        let mut full = names(&FOUNDATIONAL_INTENTS);
        full.extend((0..SusiAlphaModel::DIM - full.len()).map(|i| format!("aa_tool_{i:03}")));
        let capabilities = names(&["zz_deploy", "zz_backup", "aa_tool_000"]);
        let supported: std::collections::HashSet<String> =
            ["aa_tool_118".to_string()].into_iter().collect();
        let wanted = names(&["ZZ_Deploy", "zz_backup", "not_a_capability", "status"]);

        let (vocab, reclaimed) =
            admit_staged_actions(full.clone(), &wanted, &capabilities, &supported);
        assert_eq!(vocab.len(), SusiAlphaModel::DIM);
        // Highest-index unsupported filler slots are reused, in order.
        assert_eq!(
            reclaimed,
            [
                ("aa_tool_117".to_string(), "zz_deploy".to_string()),
                ("aa_tool_116".to_string(), "zz_backup".to_string())
            ]
        );
        assert!(
            vocab.contains(&"aa_tool_118".to_string()),
            "supported slot kept"
        );
        assert!(FOUNDATIONAL_INTENTS
            .iter()
            .all(|f| vocab.contains(&f.to_string())));
        assert!(!vocab.contains(&"not_a_capability".to_string()));
        // Indices of untouched actions are stable.
        for (i, slot) in full.iter().enumerate() {
            if !reclaimed.iter().any(|(old, _)| old == slot) {
                assert_eq!(&vocab[i], slot);
            }
        }

        // Room left: capabilities are appended, nothing reclaimed.
        let (grown, none) =
            admit_staged_actions(names(&["status"]), &wanted, &capabilities, &supported);
        assert!(none.is_empty());
        assert_eq!(grown, names(&["status", "zz_deploy", "zz_backup"]));
    }

    #[test]
    fn conflicting_labels_keep_only_a_strict_majority() {
        let line = |intent: &str, action: &str| {
            format!(
                r#"{{"intent":"{intent}","action":"{action}","timestamp":1,"performance_metadata":{{"source":"tool_receipt"}}}}"#
            )
        };
        let content = [
            // One multi-tool mission: no label has a majority.
            line("fix the build", "status"),
            line("fix the build", "reason"),
            // Repeated single-tool missions: 2 of 3 is a strict majority.
            line("Why so slow", "reason"),
            line("why so slow", "reason"),
            line("why so slow", "status"),
            line("check it", "status"),
        ]
        .join("\n");
        let parsed = parse_training_entries(&content, &intents()).unwrap();
        let kept: Vec<(&str, u32)> = parsed
            .entries
            .iter()
            .map(|(entry, label)| (entry.intent.as_str(), *label))
            .collect();
        assert_eq!(
            kept,
            [("Why so slow", 0), ("why so slow", 0), ("check it", 1)]
        );
        assert_eq!(parsed.skipped, 3);
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
