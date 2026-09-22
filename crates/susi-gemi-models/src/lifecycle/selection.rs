//! Model selection, preference scoring, and active engine/model overrides.

use dashmap::DashMap;
use std::fs;
use std::path::{Path, PathBuf};
use susi_error::EaiResult;
use susi_sandbox::manager::ModelInfo;

use crate::hardware::HardwareProfiler;

use super::types::ModelProvenance;
use super::ModelManager;

impl ModelManager {
    pub fn set_selected_model(model_name: &str) -> Result<String, String> {
        let susi_dir = susi_paths::SusiDirs::config_dir();
        let _ = fs::create_dir_all(&susi_dir);
        let model_file = susi_dir.join("selected_model_override.txt");
        fs::write(&model_file, model_name.trim()).map_err(|e| e.to_string())?;
        Ok(format!(
            "Selected active model override set to: '{}'",
            model_name.trim()
        ))
    }

    pub fn identify_best_suited_local_model(
        workspace: &Path,
        intent: Option<crate::intent::IntentCategory>,
    ) -> Option<ModelInfo> {
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
        m: &ModelInfo,
        heuristics: &susi_sandbox::manager::ModelScoringHeuristics,
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
    ) -> Option<ModelInfo> {
        let hw = HardwareProfiler::get_profile();
        let models = Self::list_models(workspace);
        let local_models: Vec<ModelInfo> = models
            .into_iter()
            .filter(|m| m.is_local() && !m.model_id().contains("native"))
            .filter(|m| {
                let path = PathBuf::from(m.model_id());
                path.extension().and_then(|ext| ext.to_str()) == Some("gguf")
                    && !Self::cooling_down(m.model_id())
                    && Self::valid_gguf_payload(&path)
                    && Self::get_tokenizer_path(m.model_id()).is_some()
            })
            .collect();

        if local_models.is_empty() {
            return None;
        }

        let cfg = susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
        let heuristics = cfg.model_scoring_heuristics();

        // Resolve every candidate's size up front: size_preference_score
        // needs the min/max across ALL candidates to place a percentile
        // target between them, which means every size must be known
        // before scoring the first one.
        let sized_models: Vec<(ModelInfo, f32)> = local_models
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
        let min_size_gb = sized_models
            .iter()
            .map(|(_, s)| *s)
            .fold(f32::INFINITY, f32::min);
        let max_size_gb = sized_models
            .iter()
            .map(|(_, s)| *s)
            .fold(f32::NEG_INFINITY, f32::max);

        let vram_budget_gb = hw.gpu_vram_gb as f32;
        let mut scored_models: Vec<(f32, ModelInfo)> = Vec::new();

        for (m, model_size_gb) in sized_models {
            let mut score = 0.0f32;
            score +=
                Self::size_preference_score(model_size_gb, min_size_gb, max_size_gb, complexity);
            if hw.acceleration_active && vram_budget_gb > 0.0 && model_size_gb <= vram_budget_gb {
                score += 20.0;
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
                    let cfg = susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
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
        let cfg = susi_sandbox::manager::SusiConfig::load(&global_dir).unwrap_or_default();
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
            for entry in entries.flatten() {
                let path = entry.path();
                if path.to_string_lossy().contains(model_id) && path.is_file() {
                    return Some(path);
                }
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
        if model_path
            .to_string_lossy()
            .contains("susi-native-synthesis")
        {
            return Ok(());
        }

        let prov_file = model_path.with_extension("provenance.json");
        if !prov_file.exists() {
            return Ok(());
        }

        let prov_content = fs::read_to_string(&prov_file)
            .map_err(|_| susi_error::EaiError::governance("Failed to read model provenance"))?;
        let provenance: ModelProvenance = serde_json::from_str(&prov_content)
            .map_err(|_| susi_error::EaiError::governance("Malformed model provenance"))?;

        if let Some(trusted_checksum) = provenance.original_checksum {
            let actual_checksum = Self::calculate_simple_checksum(model_path)?;
            if actual_checksum != trusted_checksum {
                return Err(susi_error::EaiError::governance(format!(
                    "Model TAMPERING detected! Hash mismatch for {}",
                    model_path.display()
                )));
            }
        }

        Ok(())
    }

    pub(crate) fn valid_tokenizer(path: &Path) -> bool {
        type Entry = (u64, std::time::SystemTime, bool);
        static CACHE: std::sync::OnceLock<DashMap<PathBuf, Entry>> = std::sync::OnceLock::new();
        let Ok(meta) = path.metadata() else {
            return false;
        };
        let Ok(modified) = meta.modified() else {
            return false;
        };
        let cache = CACHE.get_or_init(DashMap::new);
        if let Some(entry) = cache.get(path) {
            if entry.0 == meta.len() && entry.1 == modified {
                return entry.2;
            }
        }
        let valid = tokenizers::Tokenizer::from_file(path).is_ok();
        if cache.len() > 256 {
            cache.clear();
        }
        cache.insert(path.to_path_buf(), (meta.len(), modified, valid));
        valid
    }

    pub fn get_tokenizer_path(model_id: &str) -> Option<PathBuf> {
        let tokenizer_filename = susi_sandbox::manager::SusiConfig::load_global()
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
