//! Pluggable local inference engine port (`LocalEnginePlug`) and the built-in
//! pure-Rust local engine substrate (`BuiltinLocalEnginePlug`, active when
//! compiled with `--features local-engine` or registered explicitly).

use crate::engines::alpha_avail::{action_available, capability_names, FOUNDATIONAL_INTENTS};
use crate::engines::alpha_pure::{reflex_features, DIM};
use crate::susi_core::plane_bus::PlaneBus;
use crate::susi_error::{eai_err as anyhow, EaiError, EaiResult};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, RwLock};

pub const LOCAL_ENGINE_BUS_PREFIX: &str = "gpu_fabric.local_engine.";
pub const TOPIC_LOCAL_INFER_STREAM: &str = "gpu_fabric.local_engine.infer_stream";
pub const TOPIC_LOCAL_PRELOAD: &str = "gpu_fabric.local_engine.preload";
pub const TOPIC_LOCAL_UNLOAD: &str = "gpu_fabric.local_engine.unload";
pub const TOPIC_LOCAL_LOADED: &str = "gpu_fabric.local_engine.loaded_models";
pub const TOPIC_LOCAL_REUSABLE: &str = "gpu_fabric.local_engine.model_reusable";
pub const TOPIC_LOCAL_ALPHA_TRAIN: &str = "gpu_fabric.local_engine.alpha_train";
pub const TOPIC_LOCAL_ALPHA_PREDICT: &str = "gpu_fabric.local_engine.alpha_predict";
pub const TOPIC_LOCAL_ALPHA_INVENTORY: &str = "gpu_fabric.local_engine.alpha_inventory";
pub const TOPIC_LOCAL_REFLEX_SOLVE: &str = "gpu_fabric.local_engine.reflex_solve";
pub const TOPIC_LOCAL_REFLEX_ANSWER: &str = "gpu_fabric.local_engine.reflex_generate";

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
    fn alpha_train_on_staged_file(&self, global_dir: &Path, staged: &Path) -> EaiResult<String>;
    fn alpha_predict_intent(&self, global_dir: &Path, prompt: &str) -> EaiResult<String>;
    fn alpha_inventory(&self, global_dir: &Path) -> Value;
    fn reflex_try_solve(&self, intent: &str, workspace: &Path) -> EaiResult<String>;
    fn reflex_try_generate_answer(&self, intent: &str, workspace: &Path) -> EaiResult<String>;
}

static REGISTERED_PLUG: LazyLock<RwLock<Option<Arc<dyn LocalEnginePlug>>>> =
    LazyLock::new(|| RwLock::new(None));

pub fn register_local_engine_plug(plug: Arc<dyn LocalEnginePlug>) {
    *REGISTERED_PLUG.write().unwrap_or_else(|e| e.into_inner()) = Some(plug);
}

pub fn clear_local_engine_plug() {
    *REGISTERED_PLUG.write().unwrap_or_else(|e| e.into_inner()) = None;
}

pub fn local_engine_plug() -> Option<Arc<dyn LocalEnginePlug>> {
    if let Some(plug) = REGISTERED_PLUG
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
    {
        return Some(plug);
    }
    #[cfg(feature = "local-engine")]
    {
        Some(BuiltinLocalEnginePlug::shared())
    }
    #[cfg(not(feature = "local-engine"))]
    {
        // A wired plug (the commercial susi-plug-host) serves a default build.
        PlaneBus::global()
            .is_wired(TOPIC_LOCAL_INFER_STREAM)
            .then(BuiltinLocalEnginePlug::shared)
    }
}

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

pub(crate) fn inspect_gguf_header(path: &Path) -> EaiResult<(u32, u64, u64, String)> {
    let mut f = std::fs::File::open(path).map_err(|e| {
        EaiError::inference(format!("failed to open GGUF file {}: {e}", path.display()))
    })?;
    let mut h = [0u8; 24];
    f.read_exact(&mut h).map_err(|e| {
        EaiError::inference(format!("invalid GGUF header in {}: {e}", path.display()))
    })?;
    if &h[0..4] != b"GGUF" {
        return Err(EaiError::inference(format!(
            "invalid GGUF magic in {}: expected 'GGUF'",
            path.display()
        )));
    }
    let version = u32::from_le_bytes([h[4], h[5], h[6], h[7]]);
    if !(2..=3).contains(&version) {
        return Err(EaiError::inference(format!(
            "unsupported GGUF version {version} in {}",
            path.display()
        )));
    }
    let tensors = u64::from_le_bytes([h[8], h[9], h[10], h[11], h[12], h[13], h[14], h[15]]);
    let kvs = u64::from_le_bytes([h[16], h[17], h[18], h[19], h[20], h[21], h[22], h[23]]);
    let mut arch = String::from("llama");
    if kvs > 0 {
        if let Some(k) = read_gguf_str(&mut f) {
            let mut tb = [0u8; 4];
            if f.read_exact(&mut tb).is_ok()
                && k == "general.architecture"
                && u32::from_le_bytes(tb) == 8
            {
                if let Some(v) = read_gguf_str(&mut f) {
                    arch = v;
                }
            }
        }
    }
    crate::engines::runtime::InferenceHost::validate_architecture(&arch)?;
    Ok((version, tensors, kvs, arch))
}

fn read_gguf_str(r: &mut impl Read) -> Option<String> {
    let mut lb = [0u8; 8];
    r.read_exact(&mut lb).ok()?;
    let len = usize::try_from(u64::from_le_bytes(lb)).ok()?;
    if len > 65536 {
        return None;
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).ok()?;
    String::from_utf8(buf).ok()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StagedRecord {
    intent: String,
    action: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReflexCheckpoint {
    schema: String,
    intents: Vec<String>,
    prototypes: Vec<(Vec<f32>, String)>,
}

#[derive(Clone)]
pub struct TrainedAlphaReflex {
    ckpt: ReflexCheckpoint,
}

impl TrainedAlphaReflex {
    pub fn load_from_dir(global_dir: &Path) -> EaiResult<Self> {
        let alpha_filename =
            crate::susi_sandbox::manager::SusiConfig::load(global_dir)?.alpha_weights_filename();
        let bundle = global_dir
            .join("models")
            .join(&alpha_filename)
            .with_extension("bundle.json");
        let bytes = std::fs::read(&bundle).map_err(|_| {
            anyhow!(
                "no published reflex checkpoint under {}",
                global_dir.display()
            )
        })?;
        let ckpt: ReflexCheckpoint = serde_json::from_slice(&bytes)
            .map_err(|e| anyhow!("invalid reflex bundle {}: {e}", bundle.display()))?;
        Ok(Self { ckpt })
    }

    pub fn predict_intent(&self, raw: &str) -> EaiResult<String> {
        let x = reflex_features(raw.trim());
        let mut best_score = 0.0f32;
        let mut best_action: Option<&str> = None;
        for (proto, act) in &self.ckpt.prototypes {
            let dot: f32 = x.iter().zip(proto.iter()).map(|(a, b)| a * b).sum();
            if dot > best_score {
                best_score = dot;
                best_action = Some(act.as_str());
            }
        }
        let act = best_action.ok_or_else(|| anyhow!("no matching reflex prototype"))?;
        if best_score < 0.55 {
            return Err(anyhow!(
                "Prompt support ({best_score:.2}) below reflex floor"
            ));
        }
        let formatted = format!("ACTION: {act}");
        if !action_available(&formatted) {
            return Err(anyhow!("predicted action '{act}' is not available"));
        }
        Ok(formatted)
    }
}

pub(crate) fn train_alpha_reflex(global_dir: &Path, staged_file: &Path) -> EaiResult<String> {
    let content = std::fs::read_to_string(staged_file)?;
    let mut vocab: Vec<String> = FOUNDATIONAL_INTENTS
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    for cap in capability_names() {
        if vocab.len() < DIM && !vocab.iter().any(|v| v.eq_ignore_ascii_case(&cap)) {
            vocab.push(cap);
        }
    }
    let mut prototypes = Vec::new();
    for line in content.lines().filter(|l| !l.trim().is_empty()) {
        let rec: StagedRecord = serde_json::from_str(line)?;
        let act = rec
            .action
            .trim()
            .strip_prefix("ACTION: ")
            .unwrap_or(rec.action.trim())
            .to_string();
        if !vocab.iter().any(|v| v == &act) && vocab.len() < DIM {
            vocab.push(act.clone());
        }
        prototypes.push((reflex_features(&rec.intent), act));
    }
    if prototypes.is_empty() {
        return Err(anyhow!(
            "no staged reflex samples in {}",
            staged_file.display()
        ));
    }
    let count = prototypes.len();
    let alpha_filename =
        crate::susi_sandbox::manager::SusiConfig::load(global_dir)?.alpha_weights_filename();
    let models_dir = global_dir.join("models");
    std::fs::create_dir_all(&models_dir)?;
    let bundle_path = models_dir
        .join(&alpha_filename)
        .with_extension("bundle.json");
    let ckpt = ReflexCheckpoint {
        schema: "susi/reflex-bundle/v1".to_string(),
        intents: vocab,
        prototypes,
    };
    let bytes = serde_json::to_vec_pretty(&ckpt)?;
    let _ = Sha256::digest(&bytes);
    crate::susi_config::atomic_write_bytes(&bundle_path, &bytes)?;
    Ok(format!("Tier-0 reflex trained on {count} sample(s)"))
}

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
        // `model_path`: the weights core resolved for `selected_model`, so the
        // plug loads exactly that file (additive; older plugs ignore it).
        let model_path = selected_model.and_then(crate::models::ModelManager::get_model_path);
        let payload =
            json!({ "prompt": prompt, "selected_model": selected_model, "model_path": model_path });
        if let Some(res) = call_bus_plug(TOPIC_LOCAL_INFER_STREAM, payload) {
            let val = res?;
            if let Some(t) = val.get("text").and_then(Value::as_str) {
                callback(t.to_string());
                return Ok(t.to_string());
            }
        }
        Err(EaiError::inference(
            "no active local GGUF execution backend is attached",
        ))
    }

    fn preload(&self, path: &Path) -> EaiResult<Value> {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let meta = std::fs::metadata(&canonical).map_err(|e| {
            EaiError::inference(format!(
                "cannot stat model weights {}: {e}",
                canonical.display()
            ))
        })?;
        let (version, tensor_count, _, arch) = inspect_gguf_header(&canonical)?;
        RESIDENT_GGUF_MODELS
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                canonical.clone(),
                ResidentGgufEntry {
                    path: canonical.clone(),
                    architecture: arch.clone(),
                    bytes: meta.len(),
                    modified_secs: file_modified_secs(&meta),
                    loaded_at_secs: now_unix_secs(),
                },
            );
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
        let removed = RESIDENT_GGUF_MODELS
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&canonical)
            .is_some();
        Ok(
            json!({ "path": canonical, "unloaded": removed, "in_flight_users": 0, "gpu_pool": "cpu" }),
        )
    }

    fn loaded_models(&self) -> Vec<Value> {
        RESIDENT_GGUF_MODELS
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .map(|e| {
                json!({
                    "path": e.path,
                    "state": "loaded",
                    "architecture": e.architecture,
                    "bytes": e.bytes,
                    "loaded_at": e.loaded_at_secs,
                })
            })
            .collect()
    }

    fn model_is_reusable(&self, path: &Path) -> bool {
        let (Ok(canonical), Ok(meta)) = (path.canonicalize(), std::fs::metadata(path)) else {
            return false;
        };
        RESIDENT_GGUF_MODELS
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&canonical)
            .is_some_and(|e| e.bytes == meta.len() && e.modified_secs == file_modified_secs(&meta))
    }

    fn alpha_train_on_staged_file(&self, global_dir: &Path, staged: &Path) -> EaiResult<String> {
        train_alpha_reflex(global_dir, staged)
    }

    fn alpha_predict_intent(&self, global_dir: &Path, prompt: &str) -> EaiResult<String> {
        TrainedAlphaReflex::load_from_dir(global_dir)?.predict_intent(prompt)
    }

    fn alpha_inventory(&self, global_dir: &Path) -> Value {
        match TrainedAlphaReflex::load_from_dir(global_dir) {
            Ok(m) => json!({ "vocabulary_capacity": DIM, "intents": m.ckpt.intents }),
            Err(e) => json!({ "error": e.to_string(), "vocabulary_capacity": DIM }),
        }
    }

    fn reflex_try_solve(&self, intent: &str, _workspace: &Path) -> EaiResult<String> {
        let dir = susi_paths::SusiDirs::config_dir();
        TrainedAlphaReflex::load_from_dir(&dir)?.predict_intent(intent)
    }

    fn reflex_try_generate_answer(&self, intent: &str, workspace: &Path) -> EaiResult<String> {
        self.reflex_try_solve(intent, workspace)
    }
}
