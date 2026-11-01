//! Model selection, preference scoring, and active engine/model overrides.

use crate::susi_error::EaiResult;
use crate::susi_sandbox::manager::DynamicModelInfo;
use dashmap::DashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::hardware::HardwareProfiler;

use super::types::ModelProvenance;
use super::ModelManager;

/// Keep one parsed tokenizer for selection and inference together. Comparing
/// bytes also catches same-size replacements within a filesystem timestamp tick.
#[derive(Default)]
struct TokenizerCache {
    entry: Option<CachedTokenizer>,
}

struct CachedTokenizer {
    path: PathBuf,
    digest: [u8; 32],
    tokenizer: std::sync::Arc<susi_vendor_candle::tokenizers::Tokenizer>,
}

impl TokenizerCache {
    fn load(
        &mut self,
        path: &Path,
    ) -> std::io::Result<std::sync::Arc<susi_vendor_candle::tokenizers::Tokenizer>> {
        use sha2::{Digest, Sha256};
        let bytes = fs::read(path)?;
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        if let Some(entry) = &self.entry {
            if entry.path == path && entry.digest == digest {
                return Ok(std::sync::Arc::clone(&entry.tokenizer));
            }
        }
        let tokenizer = std::sync::Arc::new(
            susi_vendor_candle::tokenizers::Tokenizer::from_bytes(bytes)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?,
        );
        self.entry = Some(CachedTokenizer {
            path: path.to_owned(),
            digest,
            tokenizer: std::sync::Arc::clone(&tokenizer),
        });
        Ok(tokenizer)
    }
}

fn parsed_tokenizer(
    path: &Path,
) -> std::io::Result<std::sync::Arc<susi_vendor_candle::tokenizers::Tokenizer>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<TokenizerCache>> =
        std::sync::OnceLock::new();
    CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .load(path)
}

impl ModelManager {
    pub(crate) fn is_synthetic_model_id(model_id: &str) -> bool {
        model_id == "susi-native-synthesis"
    }

    pub(crate) fn admitted_local_candidate(model: &DynamicModelInfo) -> bool {
        if !model.is_local() || Self::is_synthetic_model_id(model.model_id()) {
            return false;
        }
        let path = PathBuf::from(model.model_id());
        path.extension().and_then(|ext| ext.to_str()) == Some("gguf")
            && !Self::cooling_down(model.model_id())
            && Self::valid_gguf_payload(&path)
            && Self::verify_model_integrity(&path).is_ok()
            && Self::get_tokenizer_path(model.model_id()).is_some_and(|tokenizer| {
                susi_vendor_models::model_license::verify_local(&tokenizer).is_ok()
            })
    }
    pub fn set_selected_model(model_name: &str) -> Result<String, String> {
        susi_vendor_models::set_selected_model_override(model_name)
    }

    pub fn identify_best_suited_local_model(
        workspace: &Path,
        intent: Option<crate::intent::IntentCategory>,
    ) -> Option<DynamicModelInfo> {
        Self::identify_best_suited_local_model_with_complexity(workspace, intent, None)
    }

    /// The size term of a candidate model's fitness score, among models
    /// that already fit the RAM budget: scores by closeness to a *target*
    /// size positioned at `complexity`'s percentile between the smallest
    /// and largest resident candidate (see `TaskComplexity::target_percentile`),
    /// rather than always rewarding the biggest model. With only two
    /// resident tiers (today's common case), `Complex`'s 0.75 percentile
    /// still lands closest to the larger one, preserving the original
    /// "prefer the biggest that fits" behavior for `None`/`Complex`
    /// callers; with more resident tiers, `Moderate` (0.5) can actually
    /// land on a real middle tier instead of ricocheting between two
    /// extremes. When every candidate is the same size (no real range to
    /// target between), falls back to magnitude-based ranking. Split out
    /// as a pure function so this targeting is unit-testable without real
    /// hardware/filesystem scanning.
    pub(crate) fn size_preference_score(
        model_size_gb: f32,
        min_size_gb: f32,
        max_size_gb: f32,
        complexity: Option<crate::intent::TaskComplexity>,
    ) -> f32 {
        let range_gb = max_size_gb - min_size_gb;
        if range_gb <= f32::EPSILON {
            return model_size_gb * 5.0;
        }
        let percentile = complexity.unwrap_or_default().target_percentile();
        let target_gb = min_size_gb + percentile * range_gb;
        let distance_gb = (model_size_gb - target_gb).abs();
        -(distance_gb * 5.0)
    }

    /// Resolves a candidate model's size in GB: real file size when it's a
    /// local file, else a name-based heuristic lookup, else a 2GB
    /// fallback. Split out from the scoring loop so
    /// `identify_best_suited_local_model_with_complexity` can resolve
    /// every candidate's size once, up front, before scoring any of
    /// them - `size_preference_score` needs the min/max across all
    /// candidates, which requires knowing every size before scoring the
    /// first one.
    pub(crate) fn resolve_model_size_gb(
        m: &DynamicModelInfo,
        heuristics: &crate::susi_sandbox::manager::ModelScoringHeuristics,
    ) -> f32 {
        let p = PathBuf::from(m.model_id());
        if p.is_file() {
            if let Ok(meta) = p.metadata() {
                return meta.len() as f32 / (1024.0 * 1024.0 * 1024.0);
            }
        }
        let name_lower = m.model_id().to_lowercase();
        for (key, val) in &heuristics.size_gb_multipliers {
            if name_lower.contains(key) {
                return *val;
            }
        }
        2.0
    }

    /// Same as `identify_best_suited_local_model`, plus a `complexity`
    /// signal: for `TaskComplexity::Simple`, smaller resident models score
    /// *higher* (inverted from the default "biggest that fits wins"), so a
    /// trivial request routes to the small baseline tier
    /// (`ModelManager::baseline_download_target` ensures one stays
    /// resident) instead of always loading the biggest model available.
    /// `None`/`Complex` preserves the original behavior exactly - existing
    /// callers that don't have a live prompt to classify (report/status
    /// generation, the provisioning check in `ensure_hardware_optimal_models`)
    /// are unaffected.
    pub fn identify_best_suited_local_model_with_complexity(
        workspace: &Path,
        intent: Option<crate::intent::IntentCategory>,
        complexity: Option<crate::intent::TaskComplexity>,
    ) -> Option<DynamicModelInfo> {
        let hw = HardwareProfiler::get_profile();
        let models = Self::list_models(workspace);
        let local_models: Vec<DynamicModelInfo> = models
            .into_iter()
            .filter(Self::admitted_local_candidate)
            .collect();

        if local_models.is_empty() {
            return None;
        }

        let cfg = crate::susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
        let heuristics = cfg.model_scoring_heuristics();

        // Resolve every candidate's size up front: size_preference_score
        // needs the min/max across ALL candidates to place a percentile
        // target between them, which means every size must be known
        // before scoring the first one.
        let sized_models: Vec<(DynamicModelInfo, f32)> = local_models
            .into_iter()
            .filter_map(|m| {
                let size_gb = Self::resolve_model_size_gb(&m, &heuristics);
                Self::fits_memory(
                    size_gb,
                    hw.available_ram_gb as f32,
                    heuristics.system_ram_buffer_gb,
                    cfg.model_lifecycle().memory_overhead_ratio,
                )
                .then_some((m, size_gb))
            })
            .collect();

        // Free VRAM (minus CUDA-context reserve), not total capacity —
        // a model that fits the card on paper still OOMs at load when
        // another process or the previous inference holds memory.
        let vram_budget_gb =
            HardwareProfiler::gpu_vram_budget_bytes() as f32 / (1024.0 * 1024.0 * 1024.0);
        // When any candidate fits VRAM, drop CPU-spill giants from the
        // default pick: a 19GB Q4 on an 8GB Ada card "fits" RAM but runs
        // at ~7 tok/s and trips cloud escalation. Keep oversize models
        // only when nothing GPU-resident is available.
        let any_vram_fit = hw.acceleration_active
            && vram_budget_gb > 0.0
            && sized_models.iter().any(|(_, size)| *size <= vram_budget_gb);
        let sized_models: Vec<(DynamicModelInfo, f32)> = if any_vram_fit {
            sized_models
                .into_iter()
                .filter(|(_, size)| *size <= vram_budget_gb)
                .collect()
        } else {
            sized_models
        };
        // The brain a mission gets must be able to call tools when the engine
        // asks it to — tool support is declared by the chat template inside
        // the GGUF, so it is knowable only post-download. Same contract as the
        // VRAM filter above: drop models whose template lacks a tools block
        // when a capable sibling exists, and keep them when none does so a
        // host whose only model predates tool templates is not stranded.
        let capable: Vec<bool> = sized_models
            .iter()
            .map(|(m, _)| Self::supports_tool_calling(&PathBuf::from(m.model_id())))
            .collect();
        let sized_models: Vec<(DynamicModelInfo, f32)> = if capable.iter().any(|c| *c) {
            sized_models
                .into_iter()
                .zip(capable)
                .filter(|(_, c)| *c)
                .map(|(ms, _)| ms)
                .collect()
        } else {
            sized_models
        };
        let min_size_gb = sized_models
            .iter()
            .map(|(_, s)| *s)
            .fold(f32::INFINITY, f32::min);
        let max_size_gb = sized_models
            .iter()
            .map(|(_, s)| *s)
            .fold(f32::NEG_INFINITY, f32::max);
        let mut scored_models: Vec<(f32, DynamicModelInfo)> = Vec::new();

        for (m, model_size_gb) in sized_models {
            let mut score = 0.0f32;
            score +=
                Self::size_preference_score(model_size_gb, min_size_gb, max_size_gb, complexity);
            if hw.acceleration_active && vram_budget_gb > 0.0 && model_size_gb <= vram_budget_gb {
                score += 50.0;
            }
            if m.provider() == "NativeCandle" {
                score += heuristics.native_candle_bonus;
            }

            // INTENT-BASED ROUTING OVERRIDE (Aspiration: Context Awareness)
            if let Some(i) = intent {
                let m_id = m.model_id().to_lowercase();
                let m_tags = m
                    .fields
                    .get("tags")
                    .and_then(|t| t.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str())
                            .map(|s| s.to_lowercase())
                            .collect::<Vec<String>>()
                    })
                    .unwrap_or_default();

                let matches = match i {
                    crate::intent::IntentCategory::Coding => {
                        m_id.contains("coder")
                            || m_id.contains("code")
                            || m_tags.contains(&"coding".to_string())
                    }
                    crate::intent::IntentCategory::Mathematics => {
                        m_id.contains("math") || m_tags.contains(&"mathematics".to_string())
                    }
                    crate::intent::IntentCategory::Reasoning => {
                        m_id.contains("instruct")
                            || m_id.contains("reason")
                            || m_tags.contains(&"reasoning".to_string())
                    }
                    crate::intent::IntentCategory::Creative => {
                        m_id.contains("chat") || m_tags.contains(&"creative".to_string())
                    }
                    crate::intent::IntentCategory::General => false,
                };
                if matches {
                    score += 25.0; // Specialization helps without reversing negative size scores.
                }
            }

            scored_models.push((score, m));
        }

        scored_models.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored_models.first().map(|(_, m)| m.clone())
    }

    pub fn get_selected_model(intent: Option<crate::intent::IntentCategory>) -> Option<String> {
        Self::get_selected_model_inner(intent, None)
    }

    /// Complexity-aware selection for a real, live user prompt: routes
    /// clearly-simple prompts to the small resident baseline model instead
    /// of always picking the biggest one available. Use this at actual
    /// inference-dispatch call sites (where a real prompt exists);
    /// `get_selected_model` remains what callers with no prompt to
    /// classify (status/report generation) should use - unaffected by
    /// this addition.
    pub fn get_selected_model_for_request(prompt: &str) -> Option<String> {
        Self::get_selected_model_for_request_with_min_complexity(prompt, None, None)
    }

    /// Same as `get_selected_model_for_request`, but never selects below
    /// `min_complexity` even if the prompt's own heuristic classification
    /// would pick something lower. This is the escalation mechanism: on a
    /// retry after `TruthTransformer` verifies a previous attempt actually
    /// failed (see `SusiMasterAgent::solve_internal`), the caller passes a
    /// floor one level above what was already tried, forcing a genuinely
    /// bigger model rather than hoping a longer retry prompt happens to
    /// cross a heuristic threshold on its own.
    pub fn get_selected_model_for_request_with_min_complexity(
        prompt: &str,
        context_words: Option<usize>,
        min_complexity: Option<crate::intent::TaskComplexity>,
    ) -> Option<String> {
        let intent = crate::intent::IntentClassifier::classify(prompt);
        let heuristic_complexity =
            crate::intent::IntentClassifier::classify_complexity(prompt, context_words);
        let complexity = Self::resolve_complexity_floor(heuristic_complexity, min_complexity);
        Self::get_selected_model_inner(Some(intent), Some(complexity))
    }

    /// Never resolves below `min_complexity`: `TaskComplexity` derives
    /// `Ord` in declared severity order (`Trivial` < ... < `VeryComplex`),
    /// so `.max()` is exactly "whichever is more demanding". Split out as
    /// a pure function so the escalation-floor logic is unit-testable
    /// without the hardware/filesystem dependencies the rest of model
    /// selection carries.
    pub(crate) fn resolve_complexity_floor(
        heuristic: crate::intent::TaskComplexity,
        min_complexity: Option<crate::intent::TaskComplexity>,
    ) -> crate::intent::TaskComplexity {
        match min_complexity {
            Some(floor) => heuristic.max(floor),
            None => heuristic,
        }
    }

    pub(crate) fn get_selected_model_inner(
        intent: Option<crate::intent::IntentCategory>,
        complexity: Option<crate::intent::TaskComplexity>,
    ) -> Option<String> {
        let override_file = susi_paths::SusiDirs::config_dir().join("selected_model_override.txt");
        if let Ok(content) = fs::read_to_string(&override_file) {
            let trimmed = content.trim();
            if !trimmed.is_empty() && trimmed != "auto" && !Self::cooling_down(trimmed) {
                if let Some(path) = Self::get_model_path(trimmed) {
                    let cfg =
                        crate::susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
                    let size = path
                        .metadata()
                        .map(|m| m.len() as f32 / 1073741824.0)
                        .unwrap_or(0.0);
                    if Self::valid_gguf_payload(&path)
                        && Self::get_tokenizer_path(trimmed).is_some()
                        && Self::fits_memory(
                            size,
                            HardwareProfiler::determine_available_ram_gb() as f32,
                            cfg.model_scoring_heuristics().system_ram_buffer_gb,
                            cfg.model_lifecycle().memory_overhead_ratio,
                        )
                    {
                        return Some(trimmed.to_string());
                    }
                }
            }
        }
        let ws = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self::identify_best_suited_local_model_with_complexity(&ws, intent, complexity)
            .map(|m| m.model_id().to_string())
    }

    pub fn get_selected_engine() -> Option<String> {
        let engine_file = susi_paths::SusiDirs::config_dir().join("selected_engine.txt");
        fs::read_to_string(&engine_file)
            .ok()
            .map(|s| s.trim().to_string())
    }

    pub fn get_active_engine_and_model(
        intent: Option<crate::intent::IntentCategory>,
    ) -> (String, String) {
        let global_dir = susi_paths::SusiDirs::config_dir();
        // Reachable on every inference/model-routing decision, not just boot:
        // a config.json torn by a concurrent writer must degrade to bundled
        // defaults here rather than panic this request's thread.
        let cfg = crate::susi_sandbox::manager::SusiConfig::load(&global_dir).unwrap_or_default();
        let model = Self::get_selected_model(intent).unwrap_or(cfg.default_model());
        let engine = Self::get_selected_engine().unwrap_or(cfg.default_engine());
        (engine, model)
    }

    pub fn get_model_path(model_id: &str) -> Option<PathBuf> {
        let p = PathBuf::from(model_id);
        if p.is_file() {
            return Some(p);
        }

        let susi_models = Self::get_models_dir();
        if let Ok(entries) = std::fs::read_dir(&susi_models) {
            // Only weight files are candidates — sibling artifacts
            // (*.download.json, *.lock, *.tokenizer.json) also contain the
            // model id in their names and must never resolve as the model.
            let mut fuzzy = None;
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("gguf") {
                    continue;
                }
                if path.file_stem().and_then(|s| s.to_str()) == Some(model_id) {
                    return Some(path);
                }
                if fuzzy.is_none() && path.to_string_lossy().contains(model_id) {
                    fuzzy = Some(path);
                }
            }
            if fuzzy.is_some() {
                return fuzzy;
            }
        }

        let ws = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let system_models = Self::scan_system_for_local_models(&ws);
        if let Some(m) = system_models
            .iter()
            .find(|m| m.name().contains(model_id) || m.model_id().contains(model_id))
        {
            return Some(PathBuf::from(m.model_id()));
        }

        None
    }

    pub fn verify_model_integrity(model_path: &Path) -> EaiResult<()> {
        susi_vendor_models::model_license::verify_local(model_path)
            .map_err(crate::susi_error::EaiError::governance)?;
        Self::verify_model_checksum(model_path)
    }

    pub(crate) fn verify_model_checksum(model_path: &Path) -> EaiResult<()> {
        if Self::is_synthetic_model_id(&model_path.to_string_lossy()) {
            return Ok(());
        }

        let prov_file = model_path.with_extension("provenance.json");
        if !prov_file.exists() {
            return Ok(());
        }

        let prov_content = fs::read_to_string(&prov_file).map_err(|_| {
            crate::susi_error::EaiError::governance("Failed to read model provenance")
        })?;
        let provenance: ModelProvenance = serde_json::from_str(&prov_content)
            .map_err(|_| crate::susi_error::EaiError::governance("Malformed model provenance"))?;

        if let Some(trusted_checksum) = provenance.original_checksum {
            // Downloads now record the publisher's digest, so this runs on
            // every cold load; hashing multi-GB weights each time is paid
            // once per unchanged file (same length and mtime) per process.
            type Stamp = (u64, std::time::SystemTime, String);
            static VERIFIED: std::sync::OnceLock<DashMap<PathBuf, Stamp>> =
                std::sync::OnceLock::new();
            let meta = fs::metadata(model_path)?;
            let stamp = (meta.len(), meta.modified()?, trusted_checksum.clone());
            let verified = VERIFIED.get_or_init(DashMap::new);
            if verified.get(model_path).is_some_and(|seen| *seen == stamp) {
                return Ok(());
            }
            let actual_checksum = Self::calculate_simple_checksum(model_path)?;
            if actual_checksum != trusted_checksum {
                verified.remove(model_path);
                return Err(crate::susi_error::EaiError::governance(format!(
                    "Model TAMPERING detected! Hash mismatch for {}",
                    model_path.display()
                )));
            }
            verified.insert(model_path.to_path_buf(), stamp);
        }

        Ok(())
    }

    pub(crate) fn valid_tokenizer(path: &Path) -> bool {
        let Ok(meta) = path.metadata() else {
            return false;
        };
        let Ok(modified) = meta.modified() else {
            return false;
        };
        super::file_verdicts::remembered("tokenizer", path, (meta.len(), modified), || {
            Self::judge_tokenizer(path)
        })
    }

    /// Selection and inference share one bounded parser cache. Errors reading
    /// the artifact remain retryable; only content failures become verdicts.
    fn judge_tokenizer(path: &Path) -> Option<bool> {
        match parsed_tokenizer(path) {
            Ok(_) => Some(true),
            Err(e) if super::file_verdicts::decided_by_content(&e) => Some(false),
            Err(_) => None,
        }
    }

    /// Resolve the same validated tokenizer used during model selection without
    /// parsing its JSON twice. The cache retains at most one tokenizer.
    pub fn load_tokenizer(
        path: &Path,
    ) -> EaiResult<std::sync::Arc<susi_vendor_candle::tokenizers::Tokenizer>> {
        susi_vendor_models::model_license::verify_local(path)
            .map_err(crate::susi_error::EaiError::governance)?;
        parsed_tokenizer(path)
            .map_err(|e| crate::susi_error::EaiError::inference(format!("Tokenizer Error: {e}")))
    }

    pub fn get_tokenizer_path(model_id: &str) -> Option<PathBuf> {
        let tokenizer_filename = crate::susi_sandbox::manager::SusiConfig::load_global()
            .unwrap_or_default()
            .tokenizer_filename();
        let model_path = Self::get_model_path(model_id)?;
        let dedicated = model_path.with_extension("tokenizer.json");
        if dedicated.is_file() {
            return Self::valid_tokenizer(&dedicated).then_some(dedicated);
        }
        if let Some(parent) = model_path.parent() {
            let tokenizer_path = parent.join(&tokenizer_filename);
            if Self::valid_tokenizer(&tokenizer_path) {
                return Some(tokenizer_path);
            }
        }

        let susi_models = Self::get_models_dir();
        let default_tokenizer = susi_models.join(&tokenizer_filename);
        if Self::valid_tokenizer(&default_tokenizer) {
            return Some(default_tokenizer);
        }

        None
    }
}

#[cfg(test)]
mod selection_tests {
    use super::ModelManager;

    #[test]
    fn tokenizer_cache_reuses_bytes_and_rejects_replaced_or_missing_artifacts() {
        use susi_vendor_candle::tokenizers::{models::wordlevel::WordLevel, Tokenizer};
        let dir = std::env::temp_dir().join(format!("susi-tokenizer-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tokenizer.json");
        Tokenizer::new(WordLevel::default())
            .save(&path, false)
            .unwrap();
        let original = std::fs::read(&path).unwrap();
        let mut cache = super::TokenizerCache::default();
        let first = cache.load(&path).unwrap();
        assert!(std::sync::Arc::ptr_eq(&first, &cache.load(&path).unwrap()));

        // Same-size replacement must never return the previously parsed file.
        std::fs::write(&path, vec![b'x'; original.len()]).unwrap();
        assert!(cache.load(&path).is_err());
        std::fs::remove_file(&path).unwrap();
        assert!(cache.load(&path).is_err());
        std::fs::write(&path, original).unwrap();
        assert!(cache.load(&path).is_ok());

        // Retain only one parser even when many model candidates are scanned.
        let other = dir.join("other.json");
        Tokenizer::new(WordLevel::default())
            .save(&other, false)
            .unwrap();
        cache.load(&other).unwrap();
        assert_eq!(std::sync::Arc::strong_count(&first), 1);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_file(other).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn invalid_override_names_are_rejected_before_any_write() {
        for bad in ["", "   ", "qwen\nrm -rf", "tab\tname", &"x".repeat(600)] {
            assert!(ModelManager::set_selected_model(bad).is_err(), "{bad:?}");
        }
    }
}
