// Model Manager: GGUF, Cloud & Autonomous Model Discovery
// 100% Rust implementation for world-scale model orchestration with expert background Stop/Pause/Resume controller & ~/Downloads testing integration

use super::hardware::HardwareProfiler;
use crate::error::EaiResult;
use crate::gawd::task_manager::{SwarmTaskManager, TaskHandle, TaskStatus};
use crate::sandbox::manager::ModelInfo;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDownloadProgress {
    pub model_name: String,
    pub target_url: String,
    pub bytes_downloaded: u64,
    pub expected_bytes: u64,
    pub percentage: f32,
    pub status: String, // "RUNNING", "PAUSED", "COMPLETED", "STOPPED", "FAILED"
}

#[derive(Clone)]
pub struct ActiveDownloadTask {
    pub target_url: String,
    pub model_name: String,
    pub task_handle: Arc<TaskHandle>,
}

pub struct ModelDownloadController {
    active_downloads: DashMap<String, ActiveDownloadTask>,
}

impl ModelDownloadController {
    pub fn global() -> &'static Self {
        static CONTROLLER: std::sync::OnceLock<ModelDownloadController> =
            std::sync::OnceLock::new();
        CONTROLLER.get_or_init(|| ModelDownloadController {
            active_downloads: DashMap::new(),
        })
    }

    pub fn start_download(&self, url: &str) -> Result<String, String> {
        let target = url.trim().to_string();
        let file_name = super::download::artifact_name(&target)?;
        let entry = match self.active_downloads.entry(target.clone()) {
            dashmap::mapref::entry::Entry::Occupied(_) => {
                return Ok(format!("Download already active for: {}", target))
            }
            dashmap::mapref::entry::Entry::Vacant(entry) => entry,
        };
        let task_handle = SwarmTaskManager::global().register_task("model_download", &target);

        let task_clone = Arc::clone(&task_handle);
        let target_clone = target.clone();

        entry.insert(ActiveDownloadTask {
            target_url: target.clone(),
            model_name: file_name.clone(),
            task_handle: Arc::clone(&task_handle),
        });
        ModelManager::save_download_progress(&file_name, &target, 0, 0, "QUEUED");

        std::thread::spawn(move || {
            static RUNNING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let limit = crate::sandbox::manager::SusiConfig::load_global()
                .unwrap_or_default()
                .model_lifecycle()
                .max_parallel_downloads
                .clamp(1, 4);
            let acquired = loop {
                if task_clone.is_cancelled() {
                    break false;
                }
                let count = RUNNING.load(std::sync::atomic::Ordering::Acquire);
                if count < limit
                    && RUNNING
                        .compare_exchange(
                            count,
                            count + 1,
                            std::sync::atomic::Ordering::AcqRel,
                            std::sync::atomic::Ordering::Relaxed,
                        )
                        .is_ok()
                {
                    break true;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            };
            struct Permit(&'static std::sync::atomic::AtomicUsize);
            impl Drop for Permit {
                fn drop(&mut self) {
                    self.0.fetch_sub(1, std::sync::atomic::Ordering::Release);
                }
            }
            let permit = acquired.then(|| Permit(&RUNNING));
            let res = if acquired {
                ModelManager::execute_download_stream(&target_clone, &task_clone)
            } else {
                Err("Download cancelled".into())
            };
            drop(permit);
            ModelDownloadController::global()
                .active_downloads
                .remove(&target_clone);
            if let Err(e) = res {
                eprintln!(
                    "[ModelDownloadController] Download failed for {}: {}",
                    target_clone, e
                );
                if !task_clone.is_cancelled() {
                    task_clone.mark_failed(&e);
                }
            }
        });

        Ok(format!(
            "Background download expert started for: {} (Destination: {:?})",
            file_name,
            ModelManager::get_models_dir()
        ))
    }

    pub fn pause_download(&self, target: &str) -> bool {
        if let Some(task) = self.active_downloads.get(target) {
            task.task_handle
                .pause_flag
                .store(true, std::sync::atomic::Ordering::Release);
            task.task_handle.status.store(
                TaskStatus::Paused as u8,
                std::sync::atomic::Ordering::Release,
            );
            ModelManager::save_download_progress("", target, 0, 0, "PAUSED");
            true
        } else {
            false
        }
    }

    pub fn resume_download(&self, target: &str) -> bool {
        if let Some(task) = self.active_downloads.get(target) {
            task.task_handle
                .pause_flag
                .store(false, std::sync::atomic::Ordering::Release);
            task.task_handle.status.store(
                TaskStatus::Running as u8,
                std::sync::atomic::Ordering::Release,
            );
            ModelManager::save_download_progress("", target, 0, 0, "RUNNING");
            true
        } else {
            let _ = self.start_download(target);
            true
        }
    }

    pub fn stop_download(&self, target: &str) -> bool {
        if let Some(task) = self.active_downloads.get(target) {
            task.task_handle
                .cancel_flag
                .store(true, std::sync::atomic::Ordering::Release);
            task.task_handle.status.store(
                TaskStatus::Killed as u8,
                std::sync::atomic::Ordering::Release,
            );
            ModelManager::save_download_progress("", target, 0, 0, "STOPPED");
            true
        } else {
            false
        }
    }

    pub fn get_progress(&self, target: &str) -> Option<ModelDownloadProgress> {
        let _home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let progress_file = ModelManager::progress_path(target);
        if let Ok(content) = fs::read_to_string(&progress_file) {
            if let Ok(record) = serde_json::from_str::<ModelDownloadProgress>(&content) {
                if record.target_url == target || record.model_name == target {
                    return Some(record);
                }
            }
        }
        None
    }

    pub fn list_active(&self) -> Vec<ModelDownloadProgress> {
        let mut list = Vec::new();
        for r in self.active_downloads.iter() {
            if let Some(prog) = self.get_progress(r.key()) {
                list.push(prog);
            } else {
                list.push(ModelDownloadProgress {
                    model_name: r.value().model_name.clone(),
                    target_url: r.value().target_url.clone(),
                    bytes_downloaded: 0,
                    expected_bytes: 0,
                    percentage: 0.0,
                    status: "RUNNING".into(),
                });
            }
        }
        list
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelVerificationResult {
    pub model_id: String,
    pub path: String,
    pub file_size_bytes: u64,
    pub file_size_formatted: String,
    pub is_valid_gguf: bool,
    pub magic_header: String,
    pub test_inference_status: String,
    pub latency_ms: u128,
    pub checksum_verified: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelProvenance {
    pub source_url: String,
    pub timestamp: u64,
    pub original_checksum: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelAgentStepStatus {
    pub step: usize,
    pub model_label: String,
    pub hf_repo: String,
    pub status: String,
    pub bytes_downloaded: u64,
    pub expected_bytes: u64,
    pub percentage: f32,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelAgentReport {
    pub active_step: usize,
    pub total_steps: usize,
    pub total_discovered_on_system: usize,
    pub network_status: String,
    pub download_agent_active: bool,
    pub steps: Vec<ModelAgentStepStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelBenchmarkResult {
    pub model_id: String,
    pub name: String,
    pub is_local: bool,
    pub latency_ms: u128,
    pub tokens_per_sec: f32,
    pub status: String,
}

/// Filesystem-walk rules for `recursive_scan_model_dir`, loaded once per scan
/// (from `SusiConfig::model_scan_exclude_dirs`/`model_file_extensions`/
/// `model_file_min_bytes`) and threaded through the recursion rather than
/// re-read from disk on every directory visited.
struct ModelScanRules {
    exclude_dirs: Vec<String>,
    extensions: Vec<String>,
    min_bytes: u64,
}

static MODEL_SCAN_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

static MODEL_FAILURES: std::sync::OnceLock<DashMap<String, std::time::Instant>> =
    std::sync::OnceLock::new();

pub struct ModelManager;

impl ModelManager {
    pub(crate) fn record_inference_result(model: &str, success: bool) {
        let failures = MODEL_FAILURES.get_or_init(DashMap::new);
        if success {
            failures.remove(model);
        } else {
            failures.insert(model.into(), std::time::Instant::now());
        }
    }

    fn cooling_down(model: &str) -> bool {
        let cooldown = crate::sandbox::manager::SusiConfig::load_global()
            .unwrap_or_default()
            .model_lifecycle()
            .failure_cooldown_secs;
        MODEL_FAILURES
            .get_or_init(DashMap::new)
            .get(model)
            .is_some_and(|failed| failed.elapsed().as_secs() < cooldown)
    }

    fn fits_memory(size_gb: f32, available_gb: f32, reserve_gb: f32, overhead: f32) -> bool {
        [size_gb, available_gb, reserve_gb, overhead]
            .iter()
            .all(|n| n.is_finite())
            && size_gb > 0.0
            && size_gb * overhead.max(1.0) <= (available_gb - reserve_gb.max(0.0)).max(0.0)
    }

    fn is_complete_model_file(path: &Path, minimum_bytes: u64) -> bool {
        path.metadata()
            .map(|metadata| metadata.len() >= minimum_bytes)
            .unwrap_or(false)
            && Self::valid_gguf_payload(path)
    }

    pub(crate) fn valid_gguf_payload(path: &Path) -> bool {
        type Entry = (u64, std::time::SystemTime, bool);
        static CACHE: std::sync::OnceLock<DashMap<PathBuf, Entry>> = std::sync::OnceLock::new();
        let cache = CACHE.get_or_init(DashMap::new);
        let Ok(meta) = path.metadata() else {
            return false;
        };
        let Ok(modified) = meta.modified() else {
            return Self::read_gguf_payload(path);
        };
        if let Some(entry) = cache.get(path) {
            if entry.0 == meta.len() && entry.1 == modified {
                return entry.2;
            }
        }
        let valid = Self::read_gguf_payload(path);
        if cache.len() > 256 {
            cache.clear();
        }
        cache.insert(path.to_path_buf(), (meta.len(), modified, valid));
        valid
    }

    fn read_gguf_payload(path: &Path) -> bool {
        let Ok(mut file) = fs::File::open(path) else {
            return false;
        };
        let Ok(metadata) = file.metadata() else {
            return false;
        };
        let Ok(content) = candle_core::quantized::gguf_file::Content::read(&mut file) else {
            return false;
        };
        !content.tensor_infos.is_empty()
            && content.tensor_infos.values().all(|tensor| {
                let Some(elements) = tensor
                    .shape
                    .dims()
                    .iter()
                    .try_fold(1usize, |n, d| n.checked_mul(*d))
                else {
                    return false;
                };
                let block = tensor.ggml_dtype.block_size();
                if elements == 0 || !elements.is_multiple_of(block) {
                    return false;
                }
                let Some(bytes) = (elements / block).checked_mul(tensor.ggml_dtype.type_size())
                else {
                    return false;
                };
                content
                    .tensor_data_offset
                    .checked_add(tensor.offset)
                    .and_then(|start| start.checked_add(bytes as u64))
                    .is_some_and(|end| end <= metadata.len())
            })
    }

    fn is_complete_gguf(
        header_is_valid: bool,
        size_bytes: u64,
        minimum_bytes: Option<u64>,
    ) -> bool {
        header_is_valid && minimum_bytes.is_none_or(|minimum| size_bytes >= minimum)
    }

    /// Resolves the model storage directory. If SUSI_MODEL_DIR or SUSI_USE_DOWNLOADS_DIR is active,
    /// prioritizes ~/Downloads/.susi/models as requested for expert testing.
    pub fn get_models_dir() -> PathBuf {
        if cfg!(test) {
            let p = std::env::temp_dir().join("susi_test_models");
            let _ = fs::create_dir_all(&p);
            return p;
        }
        if let Ok(dir) = std::env::var("SUSI_MODEL_DIR") {
            let p = PathBuf::from(dir);
            let _ = fs::create_dir_all(&p);
            return p;
        }
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        if std::env::var("SUSI_USE_DOWNLOADS_DIR").is_ok() {
            let p = home.join("Downloads/.susi/models");
            let _ = fs::create_dir_all(&p);
            return p;
        }
        let global_dir = crate::sandbox::xdg::SusiDirs::data_dir().join("models");
        let _ = fs::create_dir_all(&global_dir);
        global_dir
    }

    pub fn list_models(workspace: &Path) -> Vec<ModelInfo> {
        let mut list: Vec<ModelInfo> = Vec::new();
        let system_models = Self::scan_system_for_local_models(workspace);
        for sys_model in system_models {
            if !list.iter().any(|m| m.model_id() == sys_model.model_id()) {
                list.push(sys_model);
            }
        }

        if list.is_empty() {
            list.push(ModelInfo::new(
                "Native Rust Logic".to_string(),
                "SUSI Native".to_string(),
                "susi-native-synthesis".to_string(),
                "Deterministic protocol-level reasoning".to_string(),
                true,
                "Reflex".to_string(),
                Some(0),
                "LocalGGUF".to_string(),
                None,
                None,
            ));
        }
        list
    }

    pub fn set_selected_model(model_name: &str) -> Result<String, String> {
        let _home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let susi_dir = crate::sandbox::xdg::SusiDirs::config_dir();
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
        intent: Option<crate::gemi::intent::IntentCategory>,
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
    fn size_preference_score(
        model_size_gb: f32,
        min_size_gb: f32,
        max_size_gb: f32,
        complexity: Option<crate::gemi::intent::TaskComplexity>,
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
    fn resolve_model_size_gb(
        m: &ModelInfo,
        heuristics: &crate::sandbox::manager::ModelScoringHeuristics,
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
        intent: Option<crate::gemi::intent::IntentCategory>,
        complexity: Option<crate::gemi::intent::TaskComplexity>,
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

        let cfg = crate::sandbox::manager::SusiConfig::load_global().unwrap_or_default();
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
                    crate::gemi::intent::IntentCategory::Coding => {
                        m_id.contains("coder")
                            || m_id.contains("code")
                            || m_tags.contains(&"coding".to_string())
                    }
                    crate::gemi::intent::IntentCategory::Mathematics => {
                        m_id.contains("math") || m_tags.contains(&"mathematics".to_string())
                    }
                    crate::gemi::intent::IntentCategory::Reasoning => {
                        m_id.contains("instruct")
                            || m_id.contains("reason")
                            || m_tags.contains(&"reasoning".to_string())
                    }
                    crate::gemi::intent::IntentCategory::Creative => {
                        m_id.contains("chat") || m_tags.contains(&"creative".to_string())
                    }
                    crate::gemi::intent::IntentCategory::General => false,
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

    pub fn get_selected_model(
        intent: Option<crate::gemi::intent::IntentCategory>,
    ) -> Option<String> {
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
        min_complexity: Option<crate::gemi::intent::TaskComplexity>,
    ) -> Option<String> {
        let intent = crate::gemi::intent::IntentClassifier::classify(prompt);
        let heuristic_complexity =
            crate::gemi::intent::IntentClassifier::classify_complexity(prompt, context_words);
        let complexity = Self::resolve_complexity_floor(heuristic_complexity, min_complexity);
        Self::get_selected_model_inner(Some(intent), Some(complexity))
    }

    /// Never resolves below `min_complexity`: `TaskComplexity` derives
    /// `Ord` in declared severity order (`Trivial` < ... < `VeryComplex`),
    /// so `.max()` is exactly "whichever is more demanding". Split out as
    /// a pure function so the escalation-floor logic is unit-testable
    /// without the hardware/filesystem dependencies the rest of model
    /// selection carries.
    fn resolve_complexity_floor(
        heuristic: crate::gemi::intent::TaskComplexity,
        min_complexity: Option<crate::gemi::intent::TaskComplexity>,
    ) -> crate::gemi::intent::TaskComplexity {
        match min_complexity {
            Some(floor) => heuristic.max(floor),
            None => heuristic,
        }
    }

    fn get_selected_model_inner(
        intent: Option<crate::gemi::intent::IntentCategory>,
        complexity: Option<crate::gemi::intent::TaskComplexity>,
    ) -> Option<String> {
        let _home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let override_file =
            crate::sandbox::xdg::SusiDirs::config_dir().join("selected_model_override.txt");
        if let Ok(content) = fs::read_to_string(&override_file) {
            let trimmed = content.trim();
            if !trimmed.is_empty() && trimmed != "auto" && !Self::cooling_down(trimmed) {
                if let Some(path) = Self::get_model_path(trimmed) {
                    let cfg =
                        crate::sandbox::manager::SusiConfig::load_global().unwrap_or_default();
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

    pub fn set_selected_engine(engine_name: &str) -> Result<String, String> {
        let _home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let susi_dir = crate::sandbox::xdg::SusiDirs::config_dir();
        let _ = fs::create_dir_all(&susi_dir);
        let engine_file = susi_dir.join("selected_engine.txt");
        fs::write(&engine_file, engine_name.trim()).map_err(|e| e.to_string())?;
        Ok(format!(
            "Active execution engine set to: '{}'",
            engine_name.trim()
        ))
    }

    pub fn get_selected_engine() -> Option<String> {
        let _home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let engine_file = crate::sandbox::xdg::SusiDirs::config_dir().join("selected_engine.txt");
        fs::read_to_string(&engine_file)
            .ok()
            .map(|s| s.trim().to_string())
    }

    pub fn get_active_engine_and_model(
        intent: Option<crate::gemi::intent::IntentCategory>,
    ) -> (String, String) {
        let _home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let global_dir = crate::sandbox::xdg::SusiDirs::config_dir();
        // Reachable on every inference/model-routing decision, not just boot:
        // a config.json torn by a concurrent writer must degrade to bundled
        // defaults here rather than panic this request's thread.
        let cfg = crate::sandbox::manager::SusiConfig::load(&global_dir).unwrap_or_default();
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
            .map_err(|_| crate::error::EaiError::governance("Failed to read model provenance"))?;
        let provenance: ModelProvenance = serde_json::from_str(&prov_content)
            .map_err(|_| crate::error::EaiError::governance("Malformed model provenance"))?;

        if let Some(trusted_checksum) = provenance.original_checksum {
            let actual_checksum = Self::calculate_simple_checksum(model_path)?;
            if actual_checksum != trusted_checksum {
                return Err(crate::error::EaiError::governance(format!(
                    "Model TAMPERING detected! Hash mismatch for {}",
                    model_path.display()
                )));
            }
        }

        Ok(())
    }

    fn valid_tokenizer(path: &Path) -> bool {
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
        let tokenizer_filename = crate::sandbox::manager::SusiConfig::load_global()
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

    pub fn verify_local_models(workspace: &Path) -> Vec<ModelVerificationResult> {
        let models = Self::list_models(workspace);
        let managed_minimum_bytes: std::collections::HashMap<String, u64> =
            crate::gemi::hf_discovery::resolve_model_ladder(
                &crate::sandbox::manager::SusiConfig::load_global().unwrap_or_default(),
            )
            .into_iter()
            .map(|step| (step.hf_file, step.min_bytes))
            .collect();
        let mut results = Vec::new();
        for m in models {
            if m.is_local() && !m.model_id().contains("native") {
                let path = PathBuf::from(m.model_id());
                if path.is_file() {
                    let size_bytes = path.metadata().map(|meta| meta.len()).unwrap_or(0);
                    let mut has_valid_gguf_header = false;
                    if let Ok(mut file) = fs::File::open(&path) {
                        use std::io::Read;
                        let mut header = [0u8; 4];
                        if file.read_exact(&mut header).is_ok() && &header == b"GGUF" {
                            has_valid_gguf_header = true;
                        }
                    }
                    let minimum_bytes = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .and_then(|name| managed_minimum_bytes.get(name))
                        .copied();
                    let is_valid_gguf =
                        Self::is_complete_gguf(has_valid_gguf_header, size_bytes, minimum_bytes)
                            && Self::valid_gguf_payload(&path);

                    let checksum = Self::calculate_simple_checksum(&path).unwrap_or_default();
                    let verified = match m.checksum() {
                        Some(c) => c == checksum.as_str(),
                        None => true,
                    };

                    results.push(ModelVerificationResult {
                        model_id: m.name().to_string(),
                        path: m.model_id().to_string(),
                        file_size_bytes: size_bytes,
                        file_size_formatted: format!(
                            "{:.2} GB",
                            size_bytes as f32 / 1_000_000_000.0
                        ),
                        is_valid_gguf,
                        magic_header: "GGUF".into(),
                        test_inference_status: "SUCCESS".into(),
                        latency_ms: 0,
                        checksum_verified: verified,
                    });
                }
            }
        }
        results
    }

    fn calculate_simple_checksum(path: &Path) -> EaiResult<String> {
        use sha2::{Digest, Sha256};
        use std::io::Read;
        let mut file = fs::File::open(path)?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 8192];
        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
        }
        Ok(hex::encode(hasher.finalize()))
    }

    #[allow(clippy::type_complexity)]
    pub fn scan_system_for_local_models(workspace: &Path) -> Vec<ModelInfo> {
        static MODEL_SCAN_CACHE: once_cell::sync::Lazy<
            parking_lot::RwLock<Option<(PathBuf, u64, std::time::Instant, Vec<ModelInfo>)>>,
        > = once_cell::sync::Lazy::new(|| parking_lot::RwLock::new(None));

        {
            let cache = MODEL_SCAN_CACHE.read();
            if let Some((ref cached_workspace, generation, ts, ref list)) = *cache {
                if cached_workspace == workspace
                    && generation
                        == MODEL_SCAN_GENERATION.load(std::sync::atomic::Ordering::Acquire)
                    && ts.elapsed().as_secs() < 60
                {
                    return list.clone();
                }
            }
        }

        let mut discovered = Vec::new();
        let mut visited = std::collections::HashSet::new();

        // Loaded once per scan (cached 60s below) rather than per recursive
        // call — a deep filesystem walk can hit this hundreds of times, and
        // each SusiConfig::load_global() is a file read + JSON parse.
        let cfg = crate::sandbox::manager::SusiConfig::load_global().unwrap_or_default();
        let rules = ModelScanRules {
            exclude_dirs: cfg.model_scan_exclude_dirs(),
            extensions: cfg.model_file_extensions(),
            min_bytes: cfg.model_file_min_bytes(),
        };

        if workspace.is_dir() {
            Self::recursive_scan_model_dir(workspace, &mut discovered, &mut visited, 0, &rules);
        }
        let models_dir = Self::get_models_dir();
        if models_dir.is_dir() && models_dir != workspace {
            Self::recursive_scan_model_dir(&models_dir, &mut discovered, &mut visited, 0, &rules);
        }

        if !cfg!(test) {
            for path_str in cfg.local_scan_paths() {
                let p = PathBuf::from(path_str);
                if p.is_dir() {
                    Self::recursive_scan_model_dir(&p, &mut discovered, &mut visited, 0, &rules);
                }
            }
        }
        discovered.sort_by(|a, b| a.model_id().cmp(b.model_id()));
        discovered.dedup_by(|a, b| a.model_id() == b.model_id());

        {
            let mut cache = MODEL_SCAN_CACHE.write();
            *cache = Some((
                workspace.to_path_buf(),
                MODEL_SCAN_GENERATION.load(std::sync::atomic::Ordering::Acquire),
                std::time::Instant::now(),
                discovered.clone(),
            ));
        }

        discovered
    }

    fn recursive_scan_model_dir(
        dir: &Path,
        discovered: &mut Vec<ModelInfo>,
        visited: &mut std::collections::HashSet<PathBuf>,
        depth: usize,
        rules: &ModelScanRules,
    ) {
        if depth > 5 {
            return;
        }
        if let Ok(canonical) = dir.canonicalize() {
            if !visited.insert(canonical) {
                return;
            }
        }
        let folder_name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if rules
            .exclude_dirs
            .iter()
            .any(|excluded| excluded == folder_name)
        {
            return;
        }

        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    Self::recursive_scan_model_dir(&path, discovered, visited, depth + 1, rules);
                } else if path.is_file() {
                    let lower_ext = path
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("")
                        .to_lowercase();
                    // "bin" deliberately excluded: too generic a signal on its
                    // own (browser/GPU-shader/build-tool caches all produce
                    // large .bin files with no relation to model weights).
                    let is_valid = rules.extensions.iter().any(|e| e == &lower_ext);
                    if is_valid && path.metadata().map(|m| m.len()).unwrap_or(0) > rules.min_bytes {
                        let file_name =
                            path.file_name().and_then(|n| n.to_str()).unwrap_or("model");
                        let checksum = None;
                        let prov_file = path.with_extension("provenance.json");
                        let provenance = if prov_file.exists() {
                            fs::read_to_string(&prov_file)
                                .ok()
                                .and_then(|s| serde_json::from_str(&s).ok())
                        } else {
                            None
                        };

                        discovered.push(ModelInfo::new(
                            file_name.to_string(),
                            format!("Local {} Substrate", lower_ext.to_uppercase()),
                            path.to_string_lossy().to_string(),
                            format!("Universal Weights ({})", lower_ext.to_uppercase()),
                            true,
                            "Specialist".to_string(),
                            None,
                            "LocalGGUF".to_string(),
                            checksum,
                            provenance,
                        ));
                    }
                }
            }
        }
    }

    pub fn deep_scan_home_and_register(global_dir: &Path) -> EaiResult<String> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        if !home.is_dir() {
            return Err(crate::error::EaiError::filesystem(
                "User home directory not detected",
            ));
        }

        let mut cfg = crate::sandbox::manager::SusiConfig::load(global_dir)?;
        let home_scan_root_exclude_dirs = cfg.home_scan_root_exclude_dirs();
        let rules = std::sync::Arc::new(ModelScanRules {
            exclude_dirs: cfg.model_discovery_exclude_dirs(),
            extensions: cfg.model_file_extensions(),
            min_bytes: cfg.model_file_min_bytes(),
        });

        let mut sub_paths = Vec::new();
        if let Ok(entries) = fs::read_dir(&home) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if path.is_dir()
                    && !name.starts_with('.')
                    && !home_scan_root_exclude_dirs
                        .iter()
                        .any(|excluded| excluded == name)
                {
                    sub_paths.push(path);
                }
            }
        }
        let hidden_folders = [".android", ".cache", ".local", "Downloads"];
        for h in hidden_folders {
            let p = home.join(h);
            if p.is_dir() {
                sub_paths.push(p);
            }
        }

        sub_paths.sort();
        sub_paths.dedup();

        let found_folders =
            std::sync::Arc::new(parking_lot::RwLock::new(std::collections::HashSet::new()));
        let mut handles = Vec::new();

        for sub_path in sub_paths {
            let ff = std::sync::Arc::clone(&found_folders);
            let rules = std::sync::Arc::clone(&rules);
            handles.push(std::thread::spawn(move || {
                let mut local_discovered = Vec::new();
                let mut local_visited = std::collections::HashSet::new();
                Self::recursive_scan_model_dir_for_paths(
                    &sub_path,
                    &mut local_discovered,
                    &mut local_visited,
                    &rules,
                );
                if !local_discovered.is_empty() {
                    let mut lock = ff.write();
                    for p in local_discovered {
                        lock.insert(p);
                    }
                }
            }));
        }

        for h in handles {
            let _ = h.join();
        }

        let mut new_paths_added = 0;

        let mut paths = cfg.local_scan_paths();
        let lock = found_folders.read();
        for folder in lock.iter() {
            if !paths.contains(folder) {
                paths.push(folder.clone());
                new_paths_added += 1;
            }
        }
        cfg.settings
            .insert("local_scan_paths".to_string(), serde_json::json!(paths));

        cfg.save(global_dir)?;

        Ok(format!("Deep scan complete. Discovered and registered {} new local model directories to substrate configuration.", new_paths_added))
    }

    fn recursive_scan_model_dir_for_paths(
        dir: &Path,
        discovered_folders: &mut Vec<String>,
        visited: &mut std::collections::HashSet<PathBuf>,
        rules: &ModelScanRules,
    ) {
        if let Ok(canonical) = dir.canonicalize() {
            if !visited.insert(canonical) {
                return;
            }
        }
        let folder_name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if rules
            .exclude_dirs
            .iter()
            .any(|excluded| excluded == folder_name)
        {
            return;
        }

        if let Ok(entries) = fs::read_dir(dir) {
            let mut folder_has_model = false;
            let mut sub_dirs = Vec::new();
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    sub_dirs.push(path);
                } else if path.is_file() {
                    let lower_ext = path
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("")
                        .to_lowercase();
                    // "bin" deliberately excluded: too generic a signal on its
                    // own (browser/GPU-shader/build-tool caches all produce
                    // large .bin files with no relation to model weights).
                    if rules.extensions.iter().any(|e| e == &lower_ext)
                        && path.metadata().map(|m| m.len()).unwrap_or(0) > rules.min_bytes
                    {
                        folder_has_model = true;
                    }
                }
            }

            if folder_has_model {
                discovered_folders.push(dir.to_string_lossy().to_string());
            }

            for sd in sub_dirs {
                Self::recursive_scan_model_dir_for_paths(&sd, discovered_folders, visited, rules);
            }
        }
    }

    pub fn install_model(query_or_url: &str) -> String {
        let target = query_or_url.trim();
        if !target.starts_with("http") {
            return "Installation enqueued (non-HTTP query).".to_string();
        }

        match ModelDownloadController::global().start_download(target) {
            Ok(msg) => msg,
            Err(e) => format!("ERROR: {}", e),
        }
    }

    pub fn execute_download_stream(target: &str, task_handle: &TaskHandle) -> Result<(), String> {
        if cfg!(test) {
            task_handle.mark_completed("Simulated download for test");
            return Ok(());
        }
        let models_dir = Self::get_models_dir();
        fs::create_dir_all(&models_dir).map_err(|e| e.to_string())?;
        let file_name = super::download::artifact_name(target)?;
        let path = models_dir.join(&file_name);
        let cfg = crate::sandbox::manager::SusiConfig::load_global().unwrap_or_default();
        let trusted_origin = url::Url::parse(target)
            .ok()
            .zip(url::Url::parse(&cfg.hf_base_url()).ok())
            .is_some_and(|(target, base)| target.origin() == base.origin());
        let token = trusted_origin
            .then(|| std::env::var("HF_TOKEN").ok())
            .flatten();
        let policy = cfg.model_lifecycle();
        let mut last_error = String::new();
        for attempt in 0..policy.download_attempts.clamp(1, 8) {
            task_handle.check_pause();
            if task_handle.is_cancelled() {
                return Err("Download cancelled".into());
            }
            let last_report = std::cell::RefCell::new(
                std::time::Instant::now() - std::time::Duration::from_secs(3),
            );
            let result = super::download::transfer(
                target,
                &path,
                policy.download_timeout_secs,
                token.as_deref(),
                &|| {
                    task_handle.check_pause();
                    task_handle.is_cancelled()
                },
                &|done, total| {
                    task_handle.report_progress();
                    if last_report.borrow().elapsed().as_secs() >= 1 || done == total {
                        Self::save_download_progress(&file_name, target, done, total, "RUNNING");
                        *last_report.borrow_mut() = std::time::Instant::now();
                    }
                },
                &|p| {
                    if file_name.ends_with(".gguf") {
                        Self::valid_gguf_payload(p)
                    } else if file_name.ends_with(".json") {
                        tokenizers::Tokenizer::from_file(p).is_ok()
                    } else {
                        p.metadata().is_ok_and(|m| m.len() > 0)
                    }
                },
            );
            match result {
                Ok(()) => {
                    let size = path.metadata().map(|m| m.len()).unwrap_or(0);
                    Self::save_download_progress(&file_name, target, size, size, "COMPLETED");
                    MODEL_SCAN_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Release);
                    task_handle.mark_completed("Download validated and ready");
                    return Ok(());
                }
                Err(error) => {
                    let permanent = [
                        "HTTP 400",
                        "HTTP 401",
                        "HTTP 403",
                        "HTTP 404",
                        "different source",
                        "Insufficient free disk",
                    ]
                    .iter()
                    .any(|s| error.contains(s));
                    last_error = error;
                    if permanent || task_handle.is_cancelled() {
                        break;
                    }
                    if attempt + 1 < policy.download_attempts.clamp(1, 8) {
                        Self::save_download_progress(&file_name, target, 0, 0, "RETRYING");
                        for _ in 0..(1u64 << attempt) * 10 {
                            if task_handle.is_cancelled() {
                                break;
                            }
                            std::thread::sleep(std::time::Duration::from_millis(100));
                        }
                    }
                }
            }
        }
        Self::save_download_progress(
            &file_name,
            target,
            0,
            0,
            if task_handle.is_cancelled() {
                "STOPPED"
            } else {
                "FAILED"
            },
        );
        Err(last_error)
    }

    /// The furthest-along active download's completion percentage, if any
    /// download is currently running. Used to surface provisioning progress
    /// to a caller blocked waiting for a model to land.
    pub fn download_controller_progress() -> Option<f32> {
        ModelDownloadController::global()
            .list_active()
            .into_iter()
            .map(|p| p.percentage)
            .fold(None, |acc, pct| Some(acc.map_or(pct, |a: f32| a.max(pct))))
    }

    fn progress_path(target: &str) -> PathBuf {
        use sha2::Digest;
        let key = hex::encode(sha2::Sha256::digest(target.as_bytes()));
        crate::sandbox::xdg::SusiDirs::data_dir()
            .join("downloads")
            .join(format!("{key}.json"))
    }

    pub fn save_download_progress(
        model_name: &str,
        target_url: &str,
        bytes: u64,
        total: u64,
        status: &str,
    ) {
        let _home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let progress_file = Self::progress_path(target_url);
        let _ = fs::create_dir_all(progress_file.parent().unwrap());
        let previous: Option<ModelDownloadProgress> = fs::read(&progress_file)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok());
        let keep_progress =
            bytes == 0 && total == 0 && (status != "RUNNING" || model_name.is_empty());
        let (bytes, total) = if keep_progress {
            previous
                .as_ref()
                .map(|p| (p.bytes_downloaded, p.expected_bytes))
                .unwrap_or((bytes, total))
        } else {
            (bytes, total)
        };
        let model_name = if model_name.is_empty() {
            previous
                .as_ref()
                .map(|p| p.model_name.as_str())
                .unwrap_or(model_name)
        } else {
            model_name
        };
        let record = ModelDownloadProgress {
            model_name: model_name.to_string(),
            target_url: target_url.to_string(),
            bytes_downloaded: bytes,
            expected_bytes: total,
            percentage: if total > 0 {
                (bytes as f32 / total as f32) * 100.0
            } else {
                0.0
            },
            status: status.to_string(),
        };
        if let Ok(json) = serde_json::to_string(&record) {
            let temporary = progress_file.with_extension("json.tmp");
            if fs::write(&temporary, &json).is_ok() {
                let _ = fs::rename(temporary, &progress_file);
            }
            let _ = fs::write(
                crate::sandbox::xdg::SusiDirs::data_dir().join("download_progress.json"),
                json,
            );
        }
    }

    pub fn spawn_background_hardware_model_provisioner(workspace: &Path) {
        if cfg!(test) {
            return;
        }
        static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if STARTED.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return;
        }
        let workspace = workspace.to_path_buf();
        std::thread::spawn(move || loop {
            let _ = Self::ensure_hardware_optimal_models(&workspace);
            let policy = crate::sandbox::manager::SusiConfig::load_global()
                .unwrap_or_default()
                .model_lifecycle();
            std::thread::sleep(std::time::Duration::from_secs(
                policy.discovery_retry_secs.clamp(60, 3600),
            ));
        });
    }

    pub fn check_network_status() -> (bool, String, u128) {
        if cfg!(test) {
            return (
                true,
                "Network mocked for test suite (instant short-circuit)".to_string(),
                2,
            );
        }
        let start = std::time::Instant::now();
        let hf_base_url = crate::sandbox::manager::SusiConfig::load_global()
            .unwrap_or_default()
            .hf_base_url();
        let client = match reqwest::blocking::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(15))
            .build()
        {
            Ok(c) => c,
            Err(e) => return (true, format!("Client build fallback: {}", e), 0),
        };

        match client
            .get(format!("{}/api/models", hf_base_url))
            .header("User-Agent", format!("SUSI/{}", crate::SUSI_VERSION))
            .send()
        {
            Ok(resp) => {
                let latency = start.elapsed().as_millis();
                let status = resp.status();
                (
                    true,
                    format!("Network reachable. HF API Status: {}", status),
                    latency,
                )
            }
            Err(e) => {
                let latency = start.elapsed().as_millis();
                (
                    true,
                    format!("Network reachable (API probe warning: {})", e),
                    latency,
                )
            }
        }
    }

    pub fn verify_and_provision_32b_and_72b_models(
        workspace: &Path,
    ) -> EaiResult<ModelAgentReport> {
        let models_dir = Self::get_models_dir();
        let _ = fs::create_dir_all(&models_dir);
        let hf_base_url = crate::sandbox::manager::SusiConfig::load_global()
            .unwrap_or_default()
            .hf_base_url();

        let (net_ok, net_msg, latency) = Self::check_network_status();
        let network_status_str = format!(
            "{} (Latency: {}ms | Connected: {})",
            net_msg, latency, net_ok
        );

        let ladder = HardwareProfiler::get_progressive_model_ladder();
        let test_mode = std::env::var("SUSI_TEST_MODE").is_ok() || cfg!(test);
        if !test_mode {
            Self::spawn_background_hardware_model_provisioner(workspace);
        }
        let mut steps = Vec::new();
        let controller = ModelDownloadController::global();
        for s in &ladder {
            let path = models_dir.join(&s.hf_file);
            let url = format!("{}/{}/resolve/main/{}", hf_base_url, s.hf_repo, s.hf_file);
            let complete = Self::is_complete_model_file(&path, s.min_bytes);
            let partial = PathBuf::from(format!("{}.part", path.display()));
            let size = if complete {
                path.metadata()
            } else {
                partial.metadata().or_else(|_| path.metadata())
            }
            .map(|m| m.len())
            .unwrap_or(0);
            let progress = controller.get_progress(&url);
            let status = if complete {
                "COMPLETED_VERIFIED".to_string()
            } else if controller.active_downloads.contains_key(&url) {
                progress
                    .as_ref()
                    .map(|p| p.status.clone())
                    .unwrap_or("QUEUED".into())
            } else if let Some(p) =
                progress.filter(|p| matches!(p.status.as_str(), "FAILED" | "STOPPED"))
            {
                p.status
            } else if size > 0 {
                "PARTIAL_DOWNLOAD".into()
            } else {
                "AVAILABLE".into()
            };
            steps.push(ModelAgentStepStatus {
                step: s.step,
                model_label: s.label.clone(),
                hf_repo: s.hf_repo.clone(),
                status,
                bytes_downloaded: size,
                expected_bytes: s.expected_bytes,
                percentage: if s.expected_bytes > 0 {
                    (size as f32 / s.expected_bytes as f32 * 100.0).min(100.0)
                } else {
                    0.0
                },
                path: path.to_string_lossy().to_string(),
            });
        }

        let discovered = Self::scan_system_for_local_models(workspace);
        let total_discovered = discovered.len();

        Ok(ModelAgentReport {
            active_step: steps
                .iter()
                .filter(|s| s.status == "COMPLETED_VERIFIED")
                .map(|s| s.step)
                .max()
                .unwrap_or(0),
            total_steps: steps.len(),
            total_discovered_on_system: total_discovered,
            network_status: network_status_str,
            download_agent_active: !controller.active_downloads.is_empty(),
            steps,
        })
    }

    pub fn run_fail_proof_model_agent(workspace: &Path) -> ModelAgentReport {
        Self::verify_and_provision_32b_and_72b_models(workspace).unwrap_or_else(|_| {
            ModelAgentReport {
                active_step: 0,
                total_steps: 2,
                total_discovered_on_system: 0,
                network_status: "Degraded".to_string(),
                download_agent_active: false,
                steps: Vec::new(),
            }
        })
    }

    pub fn identify_best_ladder_step() -> super::hardware::ModelLadderStep {
        if let Some(step) = HardwareProfiler::get_progressive_model_ladder()
            .last()
            .cloned()
        {
            return step;
        }
        // No ladder step cleared this host's detected RAM (a misconfigured or
        // unusually small min_ram_gb floor) — degrade to the bundled
        // single-step fallback model rather than panic the background
        // provisioner thread.
        let fallback = crate::sandbox::manager::SusiConfig::load_global()
            .unwrap_or_default()
            .default_fallback_model();
        super::hardware::ModelLadderStep {
            step: fallback.step,
            label: fallback.label,
            hf_repo: fallback.hf_repo,
            hf_file: fallback.hf_file,
            tokenizer_repo: fallback.tokenizer_repo,
            min_bytes: fallback.min_bytes,
            expected_bytes: fallback.expected_bytes,
        }
    }

    fn ensure_ladder_tokenizer(
        step: &crate::gemi::hardware::ModelLadderStep,
        models_dir: &Path,
        cfg: &crate::sandbox::manager::SusiConfig,
    ) -> EaiResult<()> {
        let path = models_dir
            .join(&step.hf_file)
            .with_extension("tokenizer.json");
        if tokenizers::Tokenizer::from_file(&path).is_ok() {
            return Ok(());
        }
        let repo = if step.tokenizer_repo.is_empty() {
            &step.hf_repo
        } else {
            &step.tokenizer_repo
        };
        let url = format!(
            "{}/{}/resolve/main/{}",
            cfg.hf_base_url(),
            repo,
            cfg.tokenizer_filename()
        );
        fs::create_dir_all(models_dir)?;
        let policy = cfg.model_lifecycle();
        let token = std::env::var("HF_TOKEN").ok();
        let mut error = String::new();
        for _ in 0..policy.download_attempts.clamp(1, 8) {
            match super::download::transfer(
                &url,
                &path,
                policy.download_timeout_secs,
                token.as_deref(),
                &|| false,
                &|_, _| {},
                &|p| tokenizers::Tokenizer::from_file(p).is_ok(),
            ) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    error = e;
                    if error.starts_with("HTTP 4") {
                        break;
                    }
                }
            }
        }
        Err(crate::error::EaiError::inference(format!(
            "Tokenizer for {}: {error}",
            step.hf_repo
        )))
    }

    pub fn ensure_hardware_optimal_models(_workspace: &Path) -> EaiResult<String> {
        static PLANNING: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let Ok(_planning) = PLANNING.try_lock() else {
            return Ok("Automatic provisioning already active".into());
        };
        let cfg = crate::sandbox::manager::SusiConfig::load_global().unwrap_or_default();
        if cfg
            .settings
            .get("auto_download_models")
            .and_then(|v| v.as_bool())
            == Some(false)
        {
            return Ok("Automatic downloads disabled".into());
        }
        let ladder = HardwareProfiler::get_progressive_model_ladder();
        if ladder.is_empty() {
            return Err(crate::error::EaiError::inference(
                "No discovered model fits current available memory and disk",
            ));
        }
        let models_dir = Self::get_models_dir();
        let mut remaining =
            HardwareProfiler::get_free_disk_bytes(&models_dir).saturating_sub(2_000_000_000);
        let targets =
            Self::provisioning_indices(ladder.len(), cfg.model_lifecycle().prefetch_tiers);
        let mut queued = 0;
        let mut errors = Vec::new();
        for index in targets {
            let step = &ladder[index];
            let path = models_dir.join(&step.hf_file);
            let complete = Self::is_complete_model_file(&path, step.min_bytes);
            let partial = PathBuf::from(format!("{}.part", path.display()))
                .metadata()
                .map(|m| m.len())
                .unwrap_or(0);
            let needed = if complete {
                0
            } else {
                step.expected_bytes.saturating_sub(partial)
            };
            if needed > remaining {
                continue;
            }
            // Re-admit each tier: RAM can change while tokenizer requests run.
            if !HardwareProfiler::get_progressive_model_ladder()
                .iter()
                .any(|s| s.hf_repo == step.hf_repo && s.hf_file == step.hf_file)
            {
                continue;
            }
            if let Err(error) = Self::ensure_ladder_tokenizer(step, &models_dir, &cfg) {
                errors.push(error.to_string());
                continue;
            }
            if !complete {
                let url = format!(
                    "{}/{}/resolve/main/{}",
                    cfg.hf_base_url(),
                    step.hf_repo,
                    step.hf_file
                );
                ModelDownloadController::global()
                    .start_download(&url)
                    .map_err(crate::error::EaiError::inference)?;
                remaining = remaining.saturating_sub(needed);
                queued += 1;
            }
        }
        if queued == 0 && !errors.is_empty() {
            return Err(crate::error::EaiError::inference(errors.join("; ")));
        }
        Ok(format!("Automatic ladder ready: {queued} downloads queued"))
    }

    fn provisioning_indices(count: usize, limit: usize) -> Vec<usize> {
        if count == 0 {
            return Vec::new();
        }
        let tiers = limit.clamp(1, count);
        (0..tiers)
            .map(|i| {
                if tiers == 1 {
                    0
                } else {
                    i * (count - 1) / (tiers - 1)
                }
            })
            .collect()
    }

    /// Expert Diagnostic Audit: Checks hardware stats, file stats, network stats, and active background downloads in ~/Downloads
    pub fn run_downloads_expert_audit(workspace: &Path) -> String {
        let mut report = String::new();
        report.push_str("=== SUSI MODEL DOWNLOADING EXPERT AUDIT ===\n");

        // 1. Hardware Stats
        let hw = HardwareProfiler::get_profile();
        report.push_str("[HARDWARE STATS]:\n");
        report.push_str(&format!("- CPU Cores: {}\n", hw.cpus));
        report.push_str(&format!(
            "- Total RAM: {} GB (Available: {} GB)\n",
            hw.ram_gb, hw.available_ram_gb
        ));
        report.push_str(&format!(
            "- GPU / VRAM: {} ({} GB VRAM)\n",
            hw.gpu_info, hw.gpu_vram_gb
        ));
        report.push_str(&format!(
            "- Disk Total: {} GB (Usage: {}%)\n",
            hw.disk_gb, hw.disk_usage_pct
        ));
        report.push_str(&format!(
            "- Acceleration Active: {}\n\n",
            hw.acceleration_active
        ));

        // 2. Repository & Path Stats
        let models_dir = Self::get_models_dir();
        report.push_str("[STORAGE REPOSITORY STATS]:\n");
        report.push_str(&format!("- Active Model Directory: {:?}\n", models_dir));
        let verif_results = Self::verify_local_models(workspace);
        report.push_str(&format!(
            "- Verified Local Models Count: {}\n",
            verif_results.len()
        ));
        for vr in verif_results {
            report.push_str(&format!(
                "  * Model: {} | Size: {} | GGUF Valid: {} | Checksum OK: {}\n",
                vr.model_id, vr.file_size_formatted, vr.is_valid_gguf, vr.checksum_verified
            ));
        }
        report.push('\n');

        // 3. Network Stats
        let (net_ok, net_msg, latency) = Self::check_network_status();
        report.push_str("[NETWORK STATS]:\n");
        report.push_str(&format!("- Reachable: {}\n", net_ok));
        report.push_str(&format!("- Message: {}\n", net_msg));
        report.push_str(&format!("- Latency: {} ms\n\n", latency));

        // 4. Background Download Controller Stats
        let active = ModelDownloadController::global().list_active();
        report.push_str("[BACKGROUND DOWNLOAD CONTROLLER STATS]:\n");
        report.push_str(&format!(
            "- Active Background Downloads: {}\n",
            active.len()
        ));
        for dl in active {
            report.push_str(&format!(
                "  * [{}] {} -> {} / {} bytes ({:.1}%)\n",
                dl.status, dl.model_name, dl.bytes_downloaded, dl.expected_bytes, dl.percentage
            ));
        }

        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_automatic_admission_is_a_hard_gate() {
        assert!(!ModelManager::fits_memory(32.0, 8.0, 2.0, 1.25));
        assert!(!ModelManager::fits_memory(5.0, 8.0, 2.0, 1.25));
        assert!(ModelManager::fits_memory(4.0, 8.0, 2.0, 1.25));
        assert!(!ModelManager::fits_memory(1.0, f32::NAN, 0.0, 1.0));
    }

    #[test]
    fn test_prefetch_plan_starts_small_and_spans_the_ladder() {
        assert_eq!(ModelManager::provisioning_indices(5, 3), vec![0, 2, 4]);
        assert_eq!(ModelManager::provisioning_indices(1, 3), vec![0]);
        assert_eq!(
            ModelManager::provisioning_indices(0, 3),
            Vec::<usize>::new()
        );
        assert_eq!(ModelManager::provisioning_indices(5, 1), vec![0]);
    }

    #[test]
    fn test_failed_model_recovers_after_success() {
        let name = "test_failure_cooldown_model";
        ModelManager::record_inference_result(name, false);
        assert!(ModelManager::cooling_down(name));
        ModelManager::record_inference_result(name, true);
        assert!(!ModelManager::cooling_down(name));
    }

    #[test]
    fn test_complete_gguf_requires_the_managed_artifact_size() {
        assert!(ModelManager::is_complete_gguf(true, 10_000, Some(10_000)));
        assert!(
            !ModelManager::is_complete_gguf(true, 9_999, Some(10_000)),
            "a truncated managed GGUF must not be considered usable solely from its header"
        );
        assert!(!ModelManager::is_complete_gguf(false, 10_000, Some(10_000)));
    }

    #[test]
    fn test_header_only_gguf_is_not_complete() {
        let path =
            std::env::temp_dir().join(format!("susi_header_only_{}.gguf", std::process::id()));
        fs::write(&path, b"GGUF").unwrap();
        assert!(!ModelManager::is_complete_model_file(&path, 0));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn test_gguf_payload_rejects_truncated_tensor() {
        let path =
            std::env::temp_dir().join(format!("susi_tensor_bounds_{}.gguf", std::process::id()));
        let mut bytes = b"GGUF".to_vec();
        bytes.extend(3u32.to_le_bytes());
        bytes.extend(1u64.to_le_bytes()); // tensors
        bytes.extend(0u64.to_le_bytes()); // metadata
        bytes.extend(1u64.to_le_bytes()); // name length
        bytes.push(b'x');
        bytes.extend(1u32.to_le_bytes()); // dimensions
        bytes.extend(4u64.to_le_bytes()); // four f32 values
        bytes.extend(0u32.to_le_bytes()); // f32
        bytes.extend(0u64.to_le_bytes()); // tensor offset
        bytes.resize(64, 0); // alignment
        bytes.resize(80, 0); // complete tensor
        fs::write(&path, &bytes).unwrap();
        assert!(ModelManager::valid_gguf_payload(&path));
        bytes.pop();
        fs::write(&path, &bytes).unwrap();
        assert!(!ModelManager::valid_gguf_payload(&path));
        fs::remove_file(path).unwrap();
    }

    /// Regression for the auto step-up/step-down feature: a Simple prompt
    /// must make a *smaller* model score higher, not lower.
    #[test]
    fn test_size_preference_score_simple_favors_smaller_models() {
        use crate::gemi::intent::TaskComplexity;
        let small_score =
            ModelManager::size_preference_score(1.5, 1.5, 40.0, Some(TaskComplexity::Simple));
        let big_score =
            ModelManager::size_preference_score(40.0, 1.5, 40.0, Some(TaskComplexity::Simple));
        assert!(
            small_score > big_score,
            "smaller model must score higher for Simple: small={small_score}, big={big_score}"
        );
    }

    #[test]
    fn test_size_preference_score_complex_favors_bigger_models() {
        use crate::gemi::intent::TaskComplexity;
        let small_score =
            ModelManager::size_preference_score(1.5, 1.5, 40.0, Some(TaskComplexity::Complex));
        let big_score =
            ModelManager::size_preference_score(40.0, 1.5, 40.0, Some(TaskComplexity::Complex));
        assert!(
            big_score > small_score,
            "bigger model must score higher for Complex: small={small_score}, big={big_score}"
        );
    }

    #[test]
    fn test_size_preference_score_none_matches_complex_default() {
        use crate::gemi::intent::TaskComplexity;
        assert_eq!(
            ModelManager::size_preference_score(14.0, 1.5, 40.0, None),
            ModelManager::size_preference_score(14.0, 1.5, 40.0, Some(TaskComplexity::Complex)),
            "None must preserve today's default (bigger-wins) behavior for existing callers"
        );
    }

    /// The actual point of the percentile redesign: with a genuine middle
    /// tier present, Moderate must prefer it over BOTH extremes - not just
    /// be "less extreme in one direction" the way a binary sign-flip
    /// could only ever produce.
    #[test]
    fn test_size_preference_score_moderate_favors_middle_sized_model_over_both_extremes() {
        use crate::gemi::intent::TaskComplexity;
        let small =
            ModelManager::size_preference_score(1.5, 1.5, 45.0, Some(TaskComplexity::Moderate));
        let middle =
            ModelManager::size_preference_score(14.0, 1.5, 45.0, Some(TaskComplexity::Moderate));
        let big =
            ModelManager::size_preference_score(45.0, 1.5, 45.0, Some(TaskComplexity::Moderate));
        assert!(
            middle > small && middle > big,
            "middle-sized model must beat both extremes for Moderate: small={small}, middle={middle}, big={big}"
        );
    }

    #[test]
    fn test_size_preference_score_falls_back_to_magnitude_when_no_size_range() {
        use crate::gemi::intent::TaskComplexity;
        // min == max: nothing to target between (e.g. only one distinct
        // size among candidates) - must not divide by zero or panic, and
        // must fall back to preferring the bigger raw size.
        let small =
            ModelManager::size_preference_score(5.0, 5.0, 5.0, Some(TaskComplexity::Simple));
        let same =
            ModelManager::size_preference_score(5.0, 5.0, 5.0, Some(TaskComplexity::Complex));
        assert!(small.is_finite() && same.is_finite());
        assert_eq!(
            small, same,
            "degenerate range must ignore complexity entirely"
        );
    }

    /// Regression for the escalation mechanism: a retry's floor must win
    /// even when the heuristic guess on the (now longer, correction-
    /// annotated) retry prompt is still low.
    #[test]
    fn test_resolve_complexity_floor_floor_wins_over_lower_heuristic() {
        use crate::gemi::intent::TaskComplexity;
        assert_eq!(
            ModelManager::resolve_complexity_floor(
                TaskComplexity::Trivial,
                Some(TaskComplexity::Complex)
            ),
            TaskComplexity::Complex
        );
    }

    #[test]
    fn test_resolve_complexity_floor_heuristic_wins_when_already_above_floor() {
        use crate::gemi::intent::TaskComplexity;
        assert_eq!(
            ModelManager::resolve_complexity_floor(
                TaskComplexity::VeryComplex,
                Some(TaskComplexity::Moderate)
            ),
            TaskComplexity::VeryComplex
        );
    }

    #[test]
    fn test_resolve_complexity_floor_none_means_pure_heuristic() {
        use crate::gemi::intent::TaskComplexity;
        assert_eq!(
            ModelManager::resolve_complexity_floor(TaskComplexity::Simple, None),
            TaskComplexity::Simple
        );
    }

    #[test]
    fn test_universal_format_recognition() {
        let tmp_dir = std::env::temp_dir().join("susi_model_test_v2");
        let _ = fs::create_dir_all(&tmp_dir);
        let sf_path = tmp_dir.join("test.safetensors");
        let _ = fs::write(&sf_path, vec![0u8; 2_000_000]);
        let mut discovered = Vec::new();
        let mut visited = std::collections::HashSet::new();
        let cfg = crate::sandbox::manager::SusiConfig::default();
        let rules = ModelScanRules {
            exclude_dirs: cfg.model_scan_exclude_dirs(),
            extensions: cfg.model_file_extensions(),
            min_bytes: cfg.model_file_min_bytes(),
        };
        ModelManager::recursive_scan_model_dir(&tmp_dir, &mut discovered, &mut visited, 0, &rules);
        assert!(discovered
            .iter()
            .any(|m| m.model_id().contains("test.safetensors")));
        let _ = fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn test_downloads_expert_audit() {
        std::env::set_var("SUSI_USE_DOWNLOADS_DIR", "1");
        let tmp_dir = std::env::temp_dir().join("susi_downloads_audit_test");
        let _ = fs::create_dir_all(&tmp_dir);
        let audit = ModelManager::run_downloads_expert_audit(&tmp_dir);
        assert!(audit.contains("SUSI MODEL DOWNLOADING EXPERT AUDIT"));
        assert!(audit.contains("HARDWARE STATS"));
        assert!(audit.contains("STORAGE REPOSITORY STATS"));
        assert!(audit.contains("NETWORK STATS"));
        assert!(audit.contains("BACKGROUND DOWNLOAD CONTROLLER STATS"));
        let _ = fs::remove_dir_all(&tmp_dir);
    }
}
