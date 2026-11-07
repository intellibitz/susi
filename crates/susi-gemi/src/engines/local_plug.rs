//! Pluggable local inference engine port (`LocalEnginePlug`) and the built-in
//! pure-Rust local engine substrate (`BuiltinLocalEnginePlug`, active when
//! compiled with `--features local-engine` or registered explicitly).
//!
//! Core GEMI exposes this port so commercial engines (`susi-gpu-fabric`) can
//! serve local GGUF inference, substrate weight caching (`InferenceHost`),
//! Tier-0 reflex distillation (`SusiAlphaModel`), and Tier-1 generative
//! reflexes (`GenerativeReflexEngine`) either in-process via
//! [`register_local_engine_plug`] or across the plane bus (`gpu_fabric.local_engine.*`).

use crate::engines::alpha_avail::{action_available, capability_names, FOUNDATIONAL_INTENTS};
use crate::engines::alpha_pure::{reflex_features, stem, ANCHOR_WORDS, DIM, SYNONYMS};
use crate::susi_core::plane_bus::PlaneBus;
use crate::susi_error::{eai_err as anyhow, EaiError, EaiResult};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

/// Topic prefix on [`PlaneBus`] that an out-of-process or commercial plug
/// (`susi-gpu-fabric`) can register to serve GEMI's local engine port.
pub const LOCAL_ENGINE_BUS_PREFIX: &str = "gpu_fabric.local_engine.";
pub const TOPIC_LOCAL_INFER_STREAM: &str = "gpu_fabric.local_engine.infer";
pub const TOPIC_LOCAL_PRELOAD: &str = "gpu_fabric.local_engine.preload";
pub const TOPIC_LOCAL_UNLOAD: &str = "gpu_fabric.local_engine.unload";
pub const TOPIC_LOCAL_LOADED: &str = "gpu_fabric.local_engine.loaded";
pub const TOPIC_LOCAL_REUSABLE: &str = "gpu_fabric.local_engine.reusable";
pub const TOPIC_LOCAL_ALPHA_TRAIN: &str = "gpu_fabric.local_engine.alpha_train";
pub const TOPIC_LOCAL_ALPHA_PREDICT: &str = "gpu_fabric.local_engine.alpha_predict";
pub const TOPIC_LOCAL_ALPHA_INVENTORY: &str = "gpu_fabric.local_engine.alpha_inventory";
pub const TOPIC_LOCAL_REFLEX_SOLVE: &str = "gpu_fabric.local_engine.reflex_solve";
pub const TOPIC_LOCAL_REFLEX_ANSWER: &str = "gpu_fabric.local_engine.reflex_answer";

/// Port contract for local inference, weight residency, Tier-0 reflex training,
/// and Tier-1 generative reflexes.
pub trait LocalEnginePlug: Send + Sync {
    fn name(&self) -> &str {
        "susi-gpu-fabric"
    }

    fn run_inference_stream(
        &self,
        prompt: &str,
        callback: &dyn Fn(String),
        selected_model: Option<&str>,
    ) -> EaiResult<String>;

    fn preload(&self, path: &Path) -> EaiResult<Value>;

    fn unload(&self, path: &Path) -> EaiResult<Value>;

    fn loaded_models(&self) -> Vec<Value>;

    fn model_is_reusable(&self, path: &Path) -> bool;

    fn alpha_train_on_staged_file(
        &self,
        global_dir: &Path,
        staged_file: &Path,
    ) -> EaiResult<String>;

    fn alpha_predict_intent(&self, global_dir: &Path, prompt: &str) -> EaiResult<String>;

    fn alpha_inventory(&self, global_dir: &Path) -> Value;

    fn reflex_try_solve(&self, intent: &str, workspace: &Path) -> EaiResult<String>;

    fn reflex_try_generate_answer(&self, intent: &str, workspace: &Path) -> EaiResult<String>;
}

static REGISTERED_PLUG: LazyLock<RwLock<Option<Arc<dyn LocalEnginePlug>>>> =
    LazyLock::new(|| RwLock::new(None));

/// Register a commercial or custom [`LocalEnginePlug`] to serve GEMI's local
/// engine port in-process.
pub fn register_local_engine_plug(plug: Arc<dyn LocalEnginePlug>) {
    *REGISTERED_PLUG.write() = Some(plug);
}

/// Remove a previously registered in-process [`LocalEnginePlug`].
pub fn clear_local_engine_plug() {
    *REGISTERED_PLUG.write() = None;
}

/// Returns the active [`LocalEnginePlug`]:
/// 1. Explicitly registered in-process plug (`register_local_engine_plug`),
/// 2. Built-in pure-Rust engine when compiled with `--features local-engine`.
pub fn local_engine_plug() -> Option<Arc<dyn LocalEnginePlug>> {
    if let Some(plug) = REGISTERED_PLUG.read().clone() {
        return Some(plug);
    }
    #[cfg(feature = "local-engine")]
    {
        Some(BuiltinLocalEnginePlug::shared())
    }
    #[cfg(not(feature = "local-engine"))]
    {
        None
    }
}

/// Probe [`PlaneBus`] for a commercial plug handler listening on `topic`.
/// Returns `Some(result)` only when a handler answered the topic (rather than
/// returning an unhandled/unregistered topic error).
pub(crate) fn call_bus_plug(topic: &str, payload: Value) -> Option<EaiResult<Value>> {
    let bus = PlaneBus::global();
    if !bus.is_wired(topic) {
        return None;
    }
    Some(bus.request(topic, payload).map_err(EaiError::inference))
}

#[derive(Debug, Clone)]
struct ResidentGgufEntry {
    path: PathBuf,
    architecture: String,
    version: u32,
    tensor_count: u64,
    kv_count: u64,
    bytes: u64,
    modified_secs: u64,
    loaded_at_secs: u64,
}

static RESIDENT_GGUF_MODELS: LazyLock<RwLock<HashMap<PathBuf, ResidentGgufEntry>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn file_modified_secs(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Inspect a GGUF file's header and metadata key-value pairs to extract
/// `general.architecture` without requiring external tensor crates.
pub(crate) fn inspect_gguf_header(path: &Path) -> EaiResult<(u32, u64, u64, String)> {
    let mut file = std::fs::File::open(path).map_err(|e| {
        EaiError::inference(format!("failed to open GGUF file {}: {e}", path.display()))
    })?;
    let mut header = [0u8; 24];
    file.read_exact(&mut header).map_err(|e| {
        EaiError::inference(format!("invalid GGUF header in {}: {e}", path.display()))
    })?;
    if &header[0..4] != b"GGUF" {
        return Err(EaiError::inference(format!(
            "invalid GGUF magic in {}: expected 'GGUF'",
            path.display()
        )));
    }
    let version = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
    if !(2..=3).contains(&version) {
        return Err(EaiError::inference(format!(
            "unsupported GGUF version {version} in {}",
            path.display()
        )));
    }
    let tensor_count = u64::from_le_bytes([
        header[8], header[9], header[10], header[11], header[12], header[13], header[14],
        header[15],
    ]);
    let kv_count = u64::from_le_bytes([
        header[16], header[17], header[18], header[19], header[20], header[21], header[22],
        header[23],
    ]);

    let mut arch = String::from("llama");
    let max_scan = kv_count.min(64);
    for _ in 0..max_scan {
        let Some(key) = read_gguf_string(&mut file) else {
            break;
        };
        let Some(vtype) = read_u32(&mut file) else {
            break;
        };
        if key == "general.architecture" && vtype == 8 {
            if let Some(val) = read_gguf_string(&mut file) {
                arch = val;
            }
            break;
        }
        if !skip_gguf_value(&mut file, vtype) {
            break;
        }
    }
    crate::engines::runtime::InferenceHost::validate_architecture(&arch)?;
    Ok((version, tensor_count, kv_count, arch))
}

fn read_u32(r: &mut impl Read) -> Option<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b).ok()?;
    Some(u32::from_le_bytes(b))
}

fn read_u64(r: &mut impl Read) -> Option<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b).ok()?;
    Some(u64::from_le_bytes(b))
}

fn read_gguf_string(r: &mut impl Read) -> Option<String> {
    let len = usize::try_from(read_u64(r)?).ok()?;
    if len > 1024 * 1024 {
        return None;
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).ok()?;
    String::from_utf8(buf).ok()
}

fn skip_gguf_value(r: &mut impl Read, vtype: u32) -> bool {
    let fixed = match vtype {
        0 | 1 | 7 => Some(1usize),
        2 | 3 => Some(2usize),
        4..=6 => Some(4usize),
        10..=12 => Some(8usize),
        8 => return read_gguf_string(r).is_some(),
        9 => {
            let Some(elem_type) = read_u32(r) else {
                return false;
            };
            let Some(count) = read_u64(r) else {
                return false;
            };
            if count > 65536 {
                return false;
            }
            for _ in 0..count {
                if !skip_gguf_value(r, elem_type) {
                    return false;
                }
            }
            return true;
        }
        _ => None,
    };
    if let Some(n) = fixed {
        let mut scratch = [0u8; 8];
        r.read_exact(&mut scratch[..n]).is_ok()
    } else {
        false
    }
}

// ============================================================================
// Pure-Rust Tier-0 Reflex Trainer & Classifier (`BuiltinAlphaEngine`)
// ============================================================================

const VOCABULARY_SCHEMA: &str = "susi/reflex-vocabulary/v1";
const BUNDLE_SCHEMA: &str = "susi/reflex-bundle/v1";
const MAX_REFLEX_INTENT_CHARS: usize = 240;
const SERVE_CONFIDENCE: f32 = 0.5;
const SUPPORT_MIN: f32 = 0.6;
const TARGET_LOSS: f64 = 0.15;
const MAX_EPOCHS: usize = 350;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DistillationStagedRecord {
    intent: String,
    action: String,
    #[serde(default)]
    timestamp: u64,
    #[serde(default)]
    performance_metadata: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ActionVocabularyFile {
    schema: String,
    weights_sha256: String,
    intents: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ReflexBundleFile {
    schema: String,
    weights_file: String,
    #[serde(default)]
    previous_weights_file: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReplayEntry {
    intent: String,
    action: String,
}

/// Trained two-layer (`DIM -> DIM -> DIM`) dense reflex network.
#[derive(Clone)]
pub struct TrainedAlphaReflex {
    w1: Vec<f32>,
    b1: Vec<f32>,
    w2: Vec<f32>,
    b2: Vec<f32>,
    intents: Vec<String>,
    support_rows: Vec<(Vec<f32>, String)>,
}

impl TrainedAlphaReflex {
    pub fn load_from_dir(global_dir: &Path) -> EaiResult<Self> {
        let alpha_filename =
            crate::susi_sandbox::manager::SusiConfig::load(global_dir)?.alpha_weights_filename();
        let configured = global_dir.join("models").join(&alpha_filename);
        let bundle_file = configured.with_extension("bundle.json");
        let active_weights = if bundle_file.is_file() {
            let bytes = std::fs::read(&bundle_file)?;
            let bundle: ReflexBundleFile = serde_json::from_slice(&bytes)
                .map_err(|e| anyhow!("invalid reflex bundle {}: {e}", bundle_file.display()))?;
            if bundle.schema != BUNDLE_SCHEMA {
                return Err(anyhow!(
                    "unsupported reflex bundle schema {}",
                    bundle.schema
                ));
            }
            configured
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(bundle.weights_file)
        } else if configured.is_file() {
            configured.clone()
        } else {
            return Err(anyhow!(
                "no published reflex checkpoint under {}",
                global_dir.display()
            ));
        };

        let vocab_file = active_weights.with_extension("intents.json");
        let vocab_bytes = std::fs::read(&vocab_file)
            .map_err(|e| anyhow!("read reflex vocabulary {}: {e}", vocab_file.display()))?;
        let vocab: ActionVocabularyFile = serde_json::from_slice(&vocab_bytes)
            .map_err(|e| anyhow!("parse reflex vocabulary: {e}"))?;
        if vocab.schema != VOCABULARY_SCHEMA {
            return Err(anyhow!(
                "unsupported reflex vocabulary schema {}",
                vocab.schema
            ));
        }
        let weight_bytes = std::fs::read(&active_weights)?;
        let actual_sha = hex::encode(Sha256::digest(&weight_bytes));
        if vocab.weights_sha256 != actual_sha {
            return Err(anyhow!(
                "reflex weights/vocabulary sha256 mismatch: expected {}, got {}",
                vocab.weights_sha256,
                actual_sha
            ));
        }
        let tensors = parse_f32_safetensors(&weight_bytes)?;
        let get_tensor = |name: &str, expected: usize| -> EaiResult<Vec<f32>> {
            let v = tensors
                .get(name)
                .cloned()
                .ok_or_else(|| anyhow!("missing tensor '{name}' in reflex checkpoint"))?;
            if v.len() != expected {
                return Err(anyhow!(
                    "tensor '{name}' length {} != expected {expected}",
                    v.len()
                ));
            }
            Ok(v)
        };
        let w1 = get_tensor("reflex.weight", DIM * DIM)?;
        let b1 = get_tensor("reflex.bias", DIM)?;
        let w2 = get_tensor("reflex_out.weight", DIM * DIM)?;
        let b2 = get_tensor("reflex_out.bias", DIM)?;

        let replay_file = configured.with_extension("replay.jsonl");
        let mut support_rows = Vec::new();
        if let Ok(text) = std::fs::read_to_string(&replay_file) {
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                if let Ok(entry) = serde_json::from_str::<ReplayEntry>(line) {
                    support_rows.push((reflex_features(&entry.intent), entry.action));
                }
            }
        }
        for intent in &vocab.intents {
            support_rows.push((reflex_features(intent), intent.clone()));
        }

        Ok(Self {
            w1,
            b1,
            w2,
            b2,
            intents: vocab.intents,
            support_rows,
        })
    }

    pub fn predict_intent(&self, raw: &str) -> EaiResult<String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(anyhow!("empty prompt"));
        }
        let x = reflex_features(trimmed);
        let mut max_support = 0.0f32;
        for (row_feat, _) in &self.support_rows {
            let dot: f32 = x.iter().zip(row_feat.iter()).map(|(a, b)| a * b).sum();
            if dot > max_support {
                max_support = dot;
            }
        }
        if !self.support_rows.is_empty() && max_support < SUPPORT_MIN {
            return Err(anyhow!(
                "Prompt support ({max_support:.2}) below reflex support floor ({SUPPORT_MIN:.2})"
            ));
        }

        let probs = self.probabilities(&x);
        let mut best_idx = 0usize;
        let mut best_prob = f32::NEG_INFINITY;
        for (idx, &p) in probs.iter().take(self.intents.len()).enumerate() {
            if p > best_prob {
                best_prob = p;
                best_idx = idx;
            }
        }
        let action_name = self
            .intents
            .get(best_idx)
            .ok_or_else(|| anyhow!("predicted index out of vocabulary bounds"))?;
        if best_prob <= SERVE_CONFIDENCE {
            return Err(anyhow!("Low confidence ({best_prob:.2}) in neural reflex."));
        }
        let formatted = format!("ACTION: {action_name}");
        if !action_available(&formatted) {
            return Err(anyhow!("predicted action '{action_name}' is not available"));
        }
        Ok(formatted)
    }

    fn probabilities(&self, x: &[f32]) -> Vec<f32> {
        let mut hidden = vec![0.0f32; DIM];
        for (o, h) in hidden.iter_mut().enumerate() {
            let row = &self.w1[o * DIM..(o + 1) * DIM];
            let sum: f32 = self.b1[o] + row.iter().zip(x.iter()).map(|(w, xi)| w * xi).sum::<f32>();
            *h = sum.max(0.0);
        }
        let mut logits = vec![0.0f32; DIM];
        let mut max_l = f32::NEG_INFINITY;
        for (o, l) in logits.iter_mut().enumerate() {
            let row = &self.w2[o * DIM..(o + 1) * DIM];
            let sum: f32 = self.b2[o]
                + row
                    .iter()
                    .zip(hidden.iter())
                    .map(|(w, hi)| w * hi)
                    .sum::<f32>();
            *l = sum;
            if sum > max_l {
                max_l = sum;
            }
        }
        let exps: Vec<f32> = logits.iter().map(|l| (l - max_l).exp()).collect();
        let total: f32 = exps.iter().sum::<f32>().max(1e-20);
        exps.into_iter().map(|e| e / total).collect()
    }
}

pub(crate) fn train_alpha_reflex(global_dir: &Path, staged_file: &Path) -> EaiResult<String> {
    let content = std::fs::read_to_string(staged_file).map_err(|e| {
        anyhow!(
            "Failed to read staged distillation file {}: {e}",
            staged_file.display()
        )
    })?;
    let mut vocab: Vec<String> = FOUNDATIONAL_INTENTS
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    for cap in capability_names() {
        if vocab.len() >= DIM {
            break;
        }
        if !vocab.iter().any(|v| v.eq_ignore_ascii_case(&cap)) {
            vocab.push(cap);
        }
    }

    let mut samples: Vec<(String, usize)> = Vec::new();
    for (line_idx, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            return Err(anyhow!(
                "empty distillation record at line {}",
                line_idx + 1
            ));
        }
        let rec: DistillationStagedRecord = serde_json::from_str(line)
            .map_err(|e| anyhow!("invalid distillation record at line {}: {e}", line_idx + 1))?;
        let intent = rec.intent.trim();
        if intent.is_empty() || intent.chars().count() > MAX_REFLEX_INTENT_CHARS {
            continue;
        }
        let failed = rec
            .performance_metadata
            .as_ref()
            .and_then(|m| m.get("outcome"))
            .and_then(Value::as_str)
            .is_some_and(|o| !matches!(o, "SUCCESS" | "COMPLETE"));
        if failed {
            continue;
        }
        let action = rec.action.trim();
        let label = if let Some(pos) = vocab.iter().position(|v| v.eq_ignore_ascii_case(action)) {
            pos
        } else if vocab.len() < DIM {
            let pos = vocab.len();
            vocab.push(action.to_string());
            pos
        } else {
            continue;
        };
        samples.push((intent.to_string(), label));
    }

    if samples.is_empty() {
        return Err(anyhow!("Empty distillation dataset."));
    }

    // Add synthetic primes for each vocabulary action so bare action names work
    let staged_len = samples.len();
    for (idx, action) in vocab.iter().enumerate() {
        samples.push((action.clone(), idx));
    }

    // Class-balance weights for staged samples + 1.0 for primes
    let mut counts: HashMap<usize, usize> = HashMap::new();
    for (_, l) in samples.iter().take(staged_len) {
        *counts.entry(*l).or_default() += 1;
    }
    let n_staged = staged_len as f64;
    let k_classes = counts.len().max(1) as f64;
    let mut sample_weights: Vec<f64> = samples
        .iter()
        .take(staged_len)
        .map(|(_, l)| {
            let c = counts.get(l).copied().unwrap_or(1) as f64;
            n_staged / (k_classes * c)
        })
        .collect();
    sample_weights.extend(std::iter::repeat_n(1.0, vocab.len()));

    let features: Vec<Vec<f64>> = samples
        .iter()
        .map(|(text, _)| reflex_features(text).into_iter().map(f64::from).collect())
        .collect();

    let bound = 1.0 / (DIM as f64).sqrt();
    let init_vec = |seed_tag: &str, count: usize| -> Vec<f64> {
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        for b in seed_tag.bytes() {
            seed = (seed ^ u64::from(b)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        }
        (0..count)
            .map(|i| {
                let mut z = seed.wrapping_add((i as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15));
                z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
                z ^= z >> 31;
                let frac = ((z >> 32) as u32 as f64) / f64::from(u32::MAX);
                -bound + frac * (2.0 * bound)
            })
            .collect()
    };

    let mut w1 = init_vec("reflex.weight", DIM * DIM);
    let mut b1 = init_vec("reflex.bias", DIM);
    let mut w2 = init_vec("reflex_out.weight", DIM * DIM);
    let mut b2 = init_vec("reflex_out.bias", DIM);

    let mut m_w1 = vec![0.0; DIM * DIM];
    let mut v_w1 = vec![0.0; DIM * DIM];
    let mut m_b1 = vec![0.0; DIM];
    let mut v_b1 = vec![0.0; DIM];
    let mut m_w2 = vec![0.0; DIM * DIM];
    let mut v_w2 = vec![0.0; DIM * DIM];
    let mut m_b2 = vec![0.0; DIM];
    let mut v_b2 = vec![0.0; DIM];

    let n_total = samples.len().max(1) as f64;
    let mut last_loss = f64::INFINITY;
    let mut epochs_run = 0usize;

    for epoch in 1..=MAX_EPOCHS {
        let mut gw1 = vec![0.0; DIM * DIM];
        let mut gb1 = vec![0.0; DIM];
        let mut gw2 = vec![0.0; DIM * DIM];
        let mut gb2 = vec![0.0; DIM];
        let mut loss_sum = 0.0;

        for ((x, &(_, label)), &weight) in features.iter().zip(samples.iter()).zip(&sample_weights)
        {
            let mut hidden = [0.0f64; DIM];
            for (o, h) in hidden.iter_mut().enumerate() {
                let row = &w1[o * DIM..(o + 1) * DIM];
                let mut acc = b1[o];
                for (&wv, &xv) in row.iter().zip(x.iter()) {
                    acc += wv * xv;
                }
                *h = acc.max(0.0);
            }
            let mut logits = [0.0f64; DIM];
            let mut max_l = f64::NEG_INFINITY;
            for (o, l) in logits.iter_mut().enumerate() {
                let row = &w2[o * DIM..(o + 1) * DIM];
                let mut acc = b2[o];
                for (&wv, &hv) in row.iter().zip(hidden.iter()) {
                    acc += wv * hv;
                }
                *l = acc;
                if acc > max_l {
                    max_l = acc;
                }
            }
            let mut exps = [0.0f64; DIM];
            let mut sum_exp = 0.0;
            for (e, &l) in exps.iter_mut().zip(logits.iter()) {
                let val = (l - max_l).exp();
                *e = val;
                sum_exp += val;
            }
            let prob_label = (exps[label] / sum_exp.max(1e-20)).max(1e-20);
            loss_sum += -weight * prob_label.ln();

            let scale = weight / n_total;
            let mut dlogits = [0.0f64; DIM];
            for (o, dl) in dlogits.iter_mut().enumerate() {
                let p = exps[o] / sum_exp.max(1e-20);
                let indicator = if o == label { 1.0 } else { 0.0 };
                *dl = (p - indicator) * scale;
            }

            let mut dhidden = [0.0f64; DIM];
            for (o, &dl) in dlogits.iter().enumerate() {
                if dl == 0.0 {
                    continue;
                }
                gb2[o] += dl;
                let gw2_row = &mut gw2[o * DIM..(o + 1) * DIM];
                let w2_row = &w2[o * DIM..(o + 1) * DIM];
                for i in 0..DIM {
                    gw2_row[i] += dl * hidden[i];
                    dhidden[i] += dl * w2_row[i];
                }
            }

            for o in 0..DIM {
                if hidden[o] <= 0.0 {
                    continue;
                }
                let dh = dhidden[o];
                if dh == 0.0 {
                    continue;
                }
                gb1[o] += dh;
                let gw1_row = &mut gw1[o * DIM..(o + 1) * DIM];
                for (dst, &xv) in gw1_row.iter_mut().zip(x.iter()) {
                    *dst += dh * xv;
                }
            }
        }

        last_loss = loss_sum / n_total;
        epochs_run = epoch;
        if last_loss <= TARGET_LOSS {
            break;
        }

        adamw_step(&mut w1, &gw1, &mut m_w1, &mut v_w1, epoch);
        adamw_step(&mut b1, &gb1, &mut m_b1, &mut v_b1, epoch);
        adamw_step(&mut w2, &gw2, &mut m_w2, &mut v_w2, epoch);
        adamw_step(&mut b2, &gb2, &mut m_b2, &mut v_b2, epoch);
    }

    let alpha_filename =
        crate::susi_sandbox::manager::SusiConfig::load(global_dir)?.alpha_weights_filename();
    let configured = global_dir.join("models").join(&alpha_filename);
    let models_dir = global_dir.join("models");
    std::fs::create_dir_all(&models_dir)?;

    let stem_name = configured
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("susi_alpha");
    let gen_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let weights_filename = format!("{stem_name}.{gen_ns}.{}.safetensors", std::process::id());
    let weights_path = models_dir.join(&weights_filename);

    let safetensor_bytes = serialize_f32_safetensors(&[
        ("reflex.weight", &[DIM, DIM], &w1),
        ("reflex.bias", &[DIM], &b1),
        ("reflex_out.weight", &[DIM, DIM], &w2),
        ("reflex_out.bias", &[DIM], &b2),
    ])?;
    crate::susi_config::atomic_write_bytes(&weights_path, &safetensor_bytes)?;

    let weights_sha256 = hex::encode(Sha256::digest(&safetensor_bytes));
    let vocab_payload = ActionVocabularyFile {
        schema: VOCABULARY_SCHEMA.to_string(),
        weights_sha256,
        intents: vocab.clone(),
    };
    crate::susi_config::atomic_write_bytes(
        &weights_path.with_extension("intents.json"),
        &serde_json::to_vec_pretty(&vocab_payload)?,
    )?;

    let mut replay_body = String::new();
    for (intent, label) in samples.iter().take(staged_len) {
        if let Some(action) = vocab.get(*label) {
            let line = serde_json::to_string(&ReplayEntry {
                intent: intent.clone(),
                action: action.clone(),
            })?;
            replay_body.push_str(&line);
            replay_body.push('\n');
        }
    }
    crate::susi_config::atomic_write_bytes(
        &configured.with_extension("replay.jsonl"),
        replay_body.as_bytes(),
    )?;

    let bundle_payload = ReflexBundleFile {
        schema: BUNDLE_SCHEMA.to_string(),
        weights_file: weights_filename,
        previous_weights_file: None,
    };
    crate::susi_config::atomic_write_bytes(
        &configured.with_extension("bundle.json"),
        &serde_json::to_vec_pretty(&bundle_payload)?,
    )?;

    Ok(format!(
        "Tier-0 reflex trained on {staged_len} sample(s) across {} action(s) in {epochs_run} epoch(s) (loss {last_loss:.4})",
        vocab.len()
    ))
}

fn adamw_step(w: &mut [f64], g: &[f64], m: &mut [f64], v: &mut [f64], step: usize) {
    let lr = 1e-3;
    let beta1 = 0.9_f64;
    let beta2 = 0.999_f64;
    let eps = 1e-8_f64;
    let wd = 0.01_f64;
    let t = i32::try_from(step).unwrap_or(1);
    let bc1 = 1.0 - beta1.powi(t);
    let bc2 = 1.0 - beta2.powi(t);
    for i in 0..w.len() {
        m[i] = beta1 * m[i] + (1.0 - beta1) * g[i];
        v[i] = beta2 * v[i] + (1.0 - beta2) * g[i] * g[i];
        let m_hat = m[i] / bc1;
        let v_hat = v[i] / bc2;
        w[i] -= lr * (m_hat / (v_hat.sqrt() + eps) + wd * w[i]);
    }
}

type SafetensorSpec<'a> = (&'a str, &'a [usize], &'a [f64]);

fn serialize_f32_safetensors(tensors: &[SafetensorSpec<'_>]) -> EaiResult<Vec<u8>> {
    let mut header = serde_json::Map::new();
    let mut offset = 0usize;
    let mut payload = Vec::new();
    for &(name, shape, data) in tensors {
        let byte_len = data.len() * 4;
        for &val in data {
            payload.extend_from_slice(&(val as f32).to_le_bytes());
        }
        header.insert(
            name.to_string(),
            json!({
                "dtype": "F32",
                "shape": shape,
                "data_offsets": [offset, offset + byte_len],
            }),
        );
        offset += byte_len;
    }
    let header_bytes = serde_json::to_vec(&Value::Object(header))?;
    let mut out = Vec::with_capacity(8 + header_bytes.len() + payload.len());
    out.extend_from_slice(&(header_bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(&header_bytes);
    out.extend_from_slice(&payload);
    Ok(out)
}

fn parse_f32_safetensors(bytes: &[u8]) -> EaiResult<HashMap<String, Vec<f32>>> {
    if bytes.len() < 8 {
        return Err(anyhow!("safetensors buffer shorter than 8 bytes"));
    }
    let header_len = usize::try_from(u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
    .map_err(|_| anyhow!("safetensors header length overflow"))?;
    let data_start = 8usize
        .checked_add(header_len)
        .ok_or_else(|| anyhow!("safetensors header offset overflow"))?;
    if bytes.len() < data_start {
        return Err(anyhow!("safetensors header truncated"));
    }
    let header: Value = serde_json::from_slice(&bytes[8..data_start])
        .map_err(|e| anyhow!("invalid safetensors header JSON: {e}"))?;
    let obj = header
        .as_object()
        .ok_or_else(|| anyhow!("safetensors header is not a JSON object"))?;
    let payload = &bytes[data_start..];
    let mut map = HashMap::new();
    for (k, v) in obj {
        if k == "__metadata__" {
            continue;
        }
        let offsets = v
            .get("data_offsets")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("missing data_offsets for {k}"))?;
        let start = offsets
            .first()
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| anyhow!("invalid start offset for {k}"))?;
        let end = offsets
            .get(1)
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| anyhow!("invalid end offset for {k}"))?;
        if start > end || end > payload.len() {
            return Err(anyhow!("safetensors offsets out of bounds for {k}"));
        }
        let slice = &payload[start..end];
        let mut vals = Vec::with_capacity(slice.len() / 4);
        for &chunk in slice.as_chunks::<4>().0 {
            vals.push(f32::from_le_bytes(chunk));
        }
        map.insert(k.clone(), vals);
    }
    Ok(map)
}

/// Built-in pure-Rust implementation of [`LocalEnginePlug`].
pub struct BuiltinLocalEnginePlug;

impl BuiltinLocalEnginePlug {
    pub fn shared() -> Arc<dyn LocalEnginePlug> {
        static INSTANCE: LazyLock<Arc<dyn LocalEnginePlug>> =
            LazyLock::new(|| Arc::new(BuiltinLocalEnginePlug));
        Arc::clone(&INSTANCE)
    }
}

impl LocalEnginePlug for BuiltinLocalEnginePlug {
    fn name(&self) -> &str {
        "susi-builtin-local-engine"
    }

    fn run_inference_stream(
        &self,
        prompt: &str,
        callback: &dyn Fn(String),
        selected_model: Option<&str>,
    ) -> EaiResult<String> {
        // 1. Check if a commercial handler on PlaneBus is serving local inference
        if let Some(res) = call_bus_plug(
            TOPIC_LOCAL_INFER_STREAM,
            json!({
                "prompt": prompt,
                "selected_model": selected_model,
            }),
        ) {
            let val = res?;
            if let Some(text) = val.get("text").and_then(Value::as_str) {
                callback(text.to_string());
                return Ok(text.to_string());
            }
        }

        // 2. Check if any local OpenAI-compatible engine (Ollama, llama.cpp, vLLM, LM Studio) is running
        let eco = crate::models::local_ecosystem::scan(&crate::models::local_ecosystem::HostProbe);
        for eng in eco.engines.iter().filter(|e| e.running) {
            if let Some(endpoint) = &eng.endpoint {
                let model = selected_model
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or("default")
                    .to_string();
                let provider = crate::engines::http_provider::HttpProvider {
                    name: format!("local-{}", eng.id),
                    api_base: endpoint.clone(),
                    model,
                    protocol: crate::engines::http_provider::InferenceProtocol::OpenAiCompletions,
                    api_key: String::new(),
                };
                if let Some(rt) = crate::engines::runtime::GemiEngine::provider_runtime() {
                    if let Ok(out) =
                        rt.block_on(crate::susi_core::Provider::generate(&provider, prompt))
                    {
                        if !out.trim().is_empty() {
                            callback(out.clone());
                            return Ok(out);
                        }
                    }
                }
            }
        }

        // 3. Verify resident or selected GGUF weights exist
        let resolved = selected_model
            .and_then(crate::models::ModelManager::get_model_path)
            .or_else(|| {
                crate::models::ModelManager::get_selected_model_for_request(prompt)
                    .and_then(|id| crate::models::ModelManager::get_model_path(&id))
            })
            .ok_or_else(|| {
                EaiError::inference(
                    "no selectable local GGUF model found on disk and no local HTTP engine running",
                )
            })?;
        let (_, _, _, arch) = inspect_gguf_header(&resolved)?;
        Err(EaiError::inference(format!(
            "local GGUF model '{}' ({arch}) verified on disk, but no active tensor execution backend is attached",
            resolved.display()
        )))
    }

    fn preload(&self, path: &Path) -> EaiResult<Value> {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let meta = std::fs::metadata(&canonical).map_err(|e| {
            EaiError::inference(format!(
                "cannot stat model weights {}: {e}",
                canonical.display()
            ))
        })?;
        if !meta.is_file() {
            return Err(EaiError::inference(format!(
                "model path is not a regular file: {}",
                canonical.display()
            )));
        }
        let (version, tensor_count, kv_count, arch) = inspect_gguf_header(&canonical)?;
        let entry = ResidentGgufEntry {
            path: canonical.clone(),
            architecture: arch.clone(),
            version,
            tensor_count,
            kv_count,
            bytes: meta.len(),
            modified_secs: file_modified_secs(&meta),
            loaded_at_secs: now_unix_secs(),
        };
        RESIDENT_GGUF_MODELS
            .write()
            .insert(canonical.clone(), entry);
        Ok(json!({
            "path": canonical,
            "architecture": arch,
            "gguf_version": version,
            "tensor_count": tensor_count,
            "bytes": meta.len(),
            "loaded": true,
        }))
    }

    fn unload(&self, path: &Path) -> EaiResult<Value> {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let removed = RESIDENT_GGUF_MODELS.write().remove(&canonical).is_some();
        Ok(json!({
            "path": canonical,
            "unloaded": removed,
            "in_flight_users": 0,
            "gpu_pool": "cpu",
        }))
    }

    fn loaded_models(&self) -> Vec<Value> {
        RESIDENT_GGUF_MODELS
            .read()
            .values()
            .map(|e| {
                json!({
                    "path": e.path,
                    "state": "loaded",
                    "architecture": e.architecture,
                    "gguf_version": e.version,
                    "tensor_count": e.tensor_count,
                    "kv_count": e.kv_count,
                    "bytes": e.bytes,
                    "loaded_at": e.loaded_at_secs,
                })
            })
            .collect()
    }

    fn model_is_reusable(&self, path: &Path) -> bool {
        let Ok(canonical) = path.canonicalize() else {
            return false;
        };
        let Ok(meta) = std::fs::metadata(&canonical) else {
            return false;
        };
        RESIDENT_GGUF_MODELS
            .read()
            .get(&canonical)
            .is_some_and(|e| e.bytes == meta.len() && e.modified_secs == file_modified_secs(&meta))
    }

    fn alpha_train_on_staged_file(
        &self,
        global_dir: &Path,
        staged_file: &Path,
    ) -> EaiResult<String> {
        train_alpha_reflex(global_dir, staged_file)
    }

    fn alpha_predict_intent(&self, global_dir: &Path, prompt: &str) -> EaiResult<String> {
        let model = TrainedAlphaReflex::load_from_dir(global_dir)?;
        model.predict_intent(prompt)
    }

    fn alpha_inventory(&self, global_dir: &Path) -> Value {
        match TrainedAlphaReflex::load_from_dir(global_dir) {
            Ok(model) => json!({
                "vocabulary_capacity": DIM,
                "vocabulary_len": model.intents.len(),
                "intents": model.intents,
                "support_rows": model.support_rows.len(),
            }),
            Err(err) => json!({
                "error": err.to_string(),
                "vocabulary_capacity": DIM,
            }),
        }
    }

    fn reflex_try_solve(&self, intent: &str, workspace: &Path) -> EaiResult<String> {
        let _ = workspace;
        let global_dir = susi_paths::SusiDirs::config_dir();
        let model = TrainedAlphaReflex::load_from_dir(&global_dir)?;
        model.predict_intent(intent)
    }

    fn reflex_try_generate_answer(&self, intent: &str, workspace: &Path) -> EaiResult<String> {
        self.reflex_try_solve(intent, workspace)
    }
}

/// Suppresses unused-constant warnings when referenced only by tests/diagnostics.
pub(crate) fn _lexicon_anchor_stems() -> HashSet<String> {
    ANCHOR_WORDS
        .iter()
        .map(|w| stem(w))
        .chain(SYNONYMS.iter().map(|(w, _)| stem(w)))
        .collect()
}
