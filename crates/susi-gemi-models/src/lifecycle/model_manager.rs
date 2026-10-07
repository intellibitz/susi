// Model Manager: GGUF, Cloud & Autonomous Model Discovery
// 100% Rust implementation for world-scale model orchestration with expert background Stop/Pause/Resume controller & ~/Downloads testing integration

use super::download_controller::{ModelDownloadController, ModelDownloadProgress};
use crate::susi_core::task_manager::TaskHandle;
use crate::susi_sandbox::manager::DynamicModelInfo;
use dashmap::DashMap;
use std::fs;
use std::path::{Path, PathBuf};
use susi_vendor_models::resumable_downloads::DownloadPlan;
use susi_vendor_models::transactional_download::Store;

#[derive(Debug, Clone)]
/// Filesystem-walk rules for `recursive_scan_model_dir`, loaded once per scan
/// (from `SusiConfig::model_scan_exclude_dirs`/`model_file_extensions`/
/// `model_file_min_bytes`) and threaded through the recursion rather than
/// re-read from disk on every directory visited.
pub(crate) struct ModelScanRules {
    pub(crate) exclude_dirs: Vec<String>,
    pub(crate) extensions: Vec<String>,
    pub(crate) min_bytes: u64,
}

pub(super) static MODEL_SCAN_GENERATION: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

static MODEL_FAILURES: std::sync::OnceLock<DashMap<String, std::time::Instant>> =
    std::sync::OnceLock::new();

pub struct ModelManager;

impl ModelManager {
    pub fn record_inference_result(model: &str, success: bool) {
        let failures = MODEL_FAILURES.get_or_init(DashMap::new);
        if success {
            failures.remove(model);
        } else {
            failures.insert(model.into(), std::time::Instant::now());
        }
    }

    pub(crate) fn cooling_down(model: &str) -> bool {
        let cooldown = crate::susi_sandbox::manager::SusiConfig::load_global()
            .unwrap_or_default()
            .model_lifecycle()
            .failure_cooldown_secs;
        MODEL_FAILURES
            .get_or_init(DashMap::new)
            .get(model)
            .is_some_and(|failed| failed.elapsed().as_secs() < cooldown)
    }

    pub(crate) fn fits_memory(
        size_gb: f32,
        available_gb: f32,
        reserve_gb: f32,
        overhead: f32,
    ) -> bool {
        [size_gb, available_gb, reserve_gb, overhead]
            .iter()
            .all(|n| n.is_finite())
            && size_gb > 0.0
            && size_gb * overhead.max(1.0) <= (available_gb - reserve_gb.max(0.0)).max(0.0)
    }

    pub(crate) fn is_complete_model_file(path: &Path, minimum_bytes: u64) -> bool {
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

    pub(crate) fn read_gguf_payload(path: &Path) -> bool {
        let Ok(mut file) = fs::File::open(path) else {
            return false;
        };
        let Ok(metadata) = file.metadata() else {
            return false;
        };
        let Ok(content) =
            susi_vendor_candle::candle_core::quantized::gguf_file::Content::read(&mut file)
        else {
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

    pub(crate) fn is_complete_gguf(
        header_is_valid: bool,
        size_bytes: u64,
        minimum_bytes: Option<u64>,
    ) -> bool {
        header_is_valid && minimum_bytes.is_none_or(|minimum| size_bytes >= minimum)
    }

    /// Whether a Jinja chat template carries a tools block. Detection is
    /// lexical — there is no machine-readable capability flag in GGUF — and
    /// covers the forms the supported architectures actually ship: the
    /// `tools` parameter check (llama/qwen3: `tools is not none`, `if tools`),
    /// tool-call delimiters (`<tool_call>`, `[TOOL_CALLS]`), and Mistral's
    /// `[AVAILABLE_TOOLS]`/`available_tools` convention. False positives from
    /// a stray mention in prompt prose only cost a preference point, never a
    /// wrong exclusion — the caller drops tool-less models only when a
    /// capable sibling exists.
    pub(crate) fn chat_template_supports_tools(template: &str) -> bool {
        let t = template.to_lowercase();
        [
            "tools is not none",
            "tools is defined",
            "if tools",
            "tool_call",
            "available_tools",
            "[tool",
        ]
        .iter()
        .any(|marker| t.contains(marker))
    }

    /// Reads the GGUF header's `tokenizer.chat_template` and reports whether
    /// the model can be asked for tool calls. The local engines do not emit
    /// tool-call parsing yet; this is the capability record that lets
    /// selection prefer capable models now and lets a hard `needs_tools`
    /// filter exist the day they do. Cached per (path, len, mtime) like
    /// `valid_gguf_payload` — `Content::read` stops at the metadata table but
    /// still walks every tensor-info entry, which is not free on big models.
    pub(crate) fn supports_tool_calling(path: &Path) -> bool {
        type Entry = (u64, std::time::SystemTime, bool);
        static CACHE: std::sync::OnceLock<DashMap<PathBuf, Entry>> = std::sync::OnceLock::new();
        let cache = CACHE.get_or_init(DashMap::new);
        let Ok(meta) = path.metadata() else {
            return false;
        };
        let Ok(modified) = meta.modified() else {
            return Self::read_gguf_template_supports_tools(path);
        };
        if let Some(entry) = cache.get(path) {
            if entry.0 == meta.len() && entry.1 == modified {
                return entry.2;
            }
        }
        let capable = Self::read_gguf_template_supports_tools(path);
        if cache.len() > 256 {
            cache.clear();
        }
        cache.insert(path.to_path_buf(), (meta.len(), modified, capable));
        capable
    }

    fn read_gguf_template_supports_tools(path: &Path) -> bool {
        let Ok(mut file) = fs::File::open(path) else {
            return false;
        };
        let Ok(content) =
            susi_vendor_candle::candle_core::quantized::gguf_file::Content::read(&mut file)
        else {
            return false;
        };
        content
            .metadata
            .get("tokenizer.chat_template")
            .and_then(|v| v.to_string().ok())
            .is_some_and(|t| Self::chat_template_supports_tools(t))
    }

    /// Resolves the model storage directory. If SUSI_MODEL_DIR or SUSI_USE_DOWNLOADS_DIR is active,
    /// prioritizes ~/Downloads/.susi/models as requested for expert testing.
    pub fn get_models_dir() -> PathBuf {
        if let Ok(dir) = std::env::var("SUSI_MODEL_DIR") {
            let p = PathBuf::from(dir);
            let _ = fs::create_dir_all(&p);
            return p;
        }
        if cfg!(test) {
            let p = std::env::temp_dir().join(format!("susi_test_models-{}", std::process::id()));
            let _ = fs::create_dir_all(&p);
            return p;
        }
        let home = susi_paths::SusiDirs::home_dir();
        if std::env::var("SUSI_USE_DOWNLOADS_DIR").is_ok() {
            let p = home.join("Downloads/.susi/models");
            let _ = fs::create_dir_all(&p);
            return p;
        }
        let global_dir = susi_paths::SusiDirs::data_dir().join("models");
        let _ = fs::create_dir_all(&global_dir);
        global_dir
    }

    pub fn list_models(workspace: &Path) -> Vec<DynamicModelInfo> {
        let mut list: Vec<DynamicModelInfo> = Vec::new();
        let system_models = Self::scan_system_for_local_models(workspace);
        for sys_model in system_models {
            if !list.iter().any(|m| m.model_id() == sys_model.model_id()) {
                list.push(sys_model);
            }
        }

        if list.is_empty() {
            list.push(DynamicModelInfo::new(
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

    /// Install a model in the calling process. CLI commands must use this
    /// path: a detached thread is terminated when the short-lived CLI exits,
    /// leaving a partial download while claiming installation started.
    pub fn install_model_foreground(url: &str) -> Result<String, String> {
        let target = url.trim();
        let parsed = url::Url::parse(target).map_err(|error| error.to_string())?;
        if parsed.scheme() != "https" {
            return Err("model installation requires an HTTPS URL".to_string());
        }
        let task = crate::susi_core::task_manager::SwarmTaskManager::global()
            .register_task("model_download_foreground", target);
        Self::execute_download_stream(target, &task)?;
        let file_name = crate::download::artifact_name(target)?;
        Ok(format!(
            "Installed and verified {} in {}",
            file_name,
            Self::get_models_dir().display()
        ))
    }

    pub fn execute_download_stream(target: &str, task_handle: &TaskHandle) -> Result<(), String> {
        if cfg!(test) {
            task_handle.mark_completed("Simulated download for test");
            return Ok(());
        }
        Self::execute_download_stream_in(target, task_handle, &Self::get_models_dir())
    }

    fn execute_download_stream_in(
        target: &str,
        task_handle: &TaskHandle,
        models_dir: &Path,
    ) -> Result<(), String> {
        fs::create_dir_all(models_dir).map_err(|e| e.to_string())?;
        let file_name = crate::download::artifact_name(target)?;
        let path = models_dir.join(&file_name);
        let store = Self::transactional_store(models_dir);
        fs::create_dir_all(
            store
                .staging_target(&file_name)
                .parent()
                .ok_or("transactional staging path has no parent directory")?,
        )
        .map_err(|e| e.to_string())?;
        Self::migrate_legacy_download_state(models_dir, &store, &file_name)?;
        let staging_target = store.staging_target(&file_name);
        let cfg = crate::susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
        let trusted_origin = url::Url::parse(target)
            .ok()
            .zip(url::Url::parse(&cfg.hf_base_url()).ok())
            .is_some_and(|(target, base)| target.origin() == base.origin());
        let token = trusted_origin
            .then(|| crate::susi_config::env_or_cloud_env("HF_TOKEN").ok())
            .flatten();
        let policy = cfg.model_lifecycle();
        // Asked once: the publisher's digest does not change between retries.
        let expected_sha256 = crate::download::published_sha256(target, token.as_deref());
        let mut last_error = String::new();
        for attempt in 0..policy.download_attempts.clamp(1, 8) {
            task_handle.check_pause();
            if task_handle.is_cancelled() {
                return Err("Download cancelled".into());
            }
            let last_report = std::cell::RefCell::new(
                std::time::Instant::now() - std::time::Duration::from_secs(3),
            );
            let result = crate::download::transfer(
                target,
                &staging_target,
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
                        susi_vendor_candle::tokenizers::Tokenizer::from_file(p).is_ok()
                    } else {
                        p.metadata().is_ok_and(|m| m.len() > 0)
                    }
                },
                expected_sha256.as_deref(),
            );
            match result {
                Ok(()) => {
                    let _published = Self::publish_download(
                        models_dir,
                        &file_name,
                        target,
                        expected_sha256.as_deref(),
                        &|candidate| Self::valid_download_artifact(&file_name, candidate),
                    )?;
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
                        "[PRIVACY]",
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

    fn transactional_store(models_dir: &Path) -> Store {
        Store::new(models_dir.parent().unwrap_or(models_dir))
    }

    fn migrate_legacy_download_state(
        models_dir: &Path,
        store: &Store,
        file_name: &str,
    ) -> Result<(), String> {
        let staged_target = store.staging_target(file_name);
        let legacy_part = models_dir.join(format!("{file_name}.part"));
        let staged_part = PathBuf::from(format!("{}.part", staged_target.display()));
        if legacy_part.is_file() && !staged_part.exists() {
            fs::rename(&legacy_part, &staged_part).map_err(|e| e.to_string())?;
        }
        let legacy_checkpoint = models_dir.join(format!("{file_name}.download.json"));
        let staged_checkpoint = PathBuf::from(format!("{}.download.json", staged_target.display()));
        if legacy_checkpoint.is_file() && !staged_checkpoint.exists() {
            fs::rename(legacy_checkpoint, staged_checkpoint).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn valid_download_artifact(file_name: &str, path: &Path) -> bool {
        if file_name.ends_with(".gguf") {
            Self::valid_gguf_payload(path)
        } else if file_name.ends_with(".json") {
            susi_vendor_candle::tokenizers::Tokenizer::from_file(path).is_ok()
        } else {
            path.metadata().is_ok_and(|metadata| metadata.len() > 0)
        }
    }

    /// Complete the production download transition after the streaming
    /// transport has written its validated payload to staging.
    fn publish_download(
        models_dir: &Path,
        file_name: &str,
        target: &str,
        expected_sha256: Option<&str>,
        validate: &dyn Fn(&Path) -> bool,
    ) -> Result<PathBuf, String> {
        let store = Self::transactional_store(models_dir);
        let plan = DownloadPlan {
            url: target.to_string(),
            dest: store.staging_target(file_name),
            sha256: expected_sha256.map(str::to_owned),
            size: None,
            max_retries: 1,
        };
        let report = store
            .commit_staged(file_name, &plan, validate)
            .map_err(|error| error.to_string())?;
        report
            .path
            .filter(|_| report.published)
            .ok_or_else(|| "staged model was not published after verification".to_string())
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

    pub(crate) fn progress_path(target: &str) -> PathBuf {
        use sha2::Digest;
        let key = hex::encode(sha2::Sha256::digest(target.as_bytes()));
        susi_paths::SusiDirs::data_dir()
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
        let progress_file = Self::progress_path(target_url);
        if let Some(dir) = progress_file.parent() {
            let _ = fs::create_dir_all(dir);
        }
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
            // Both mirrors are polled by other processes mid-download;
            // neither may ever be observed torn.
            let _ = crate::susi_config::atomic_write_bytes(&progress_file, json.as_bytes());
            let _ = crate::susi_config::atomic_write_bytes(
                &susi_paths::SusiDirs::data_dir().join("download_progress.json"),
                json.as_bytes(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vc_201_044_download_publish_called_by_production() {
        let root =
            std::env::temp_dir().join(format!("susi-vc-201-044-production-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let models_dir = root.join("models");
        fs::create_dir_all(&models_dir).unwrap();

        let file_name = "production-model.bin";
        let payload = b"streamed model payload";
        let store = ModelManager::transactional_store(&models_dir);
        let staged = store.staging_target(file_name);
        fs::create_dir_all(staged.parent().unwrap()).unwrap();
        fs::write(&staged, payload).unwrap();
        let checksum = susi_vendor_models::resumable_downloads::sha256_hex(payload);

        let final_path = ModelManager::publish_download(
            &models_dir,
            file_name,
            "https://models.example.test/production-model.bin",
            Some(&checksum),
            &|path| path.metadata().is_ok_and(|metadata| metadata.len() > 0),
        )
        .unwrap();

        assert_eq!(final_path, models_dir.join(file_name));
        assert_eq!(fs::read(&final_path).unwrap(), payload);
        assert!(store.is_ready(file_name));
        assert!(!staged.exists());
        assert!(store.recover().is_empty());
        let _ = fs::remove_dir_all(root);
    }

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
        use crate::intent::TaskComplexity;
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
        use crate::intent::TaskComplexity;
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
        use crate::intent::TaskComplexity;
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
        use crate::intent::TaskComplexity;
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
        use crate::intent::TaskComplexity;
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
        use crate::intent::TaskComplexity;
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
        use crate::intent::TaskComplexity;
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
        use crate::intent::TaskComplexity;
        assert_eq!(
            ModelManager::resolve_complexity_floor(TaskComplexity::Simple, None),
            TaskComplexity::Simple
        );
    }

    #[test]
    fn test_universal_format_recognition() {
        let tmp_dir =
            std::env::temp_dir().join(format!("susi_model_test_v2-{}", std::process::id()));
        let _ = fs::create_dir_all(&tmp_dir);
        let sf_path = tmp_dir.join("test.safetensors");
        let _ = fs::write(&sf_path, vec![0u8; 2_000_000]);
        let mut discovered = Vec::new();
        let mut visited = std::collections::HashSet::new();
        let cfg = crate::susi_sandbox::manager::SusiConfig::default();
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
        let tmp_dir =
            std::env::temp_dir().join(format!("susi_downloads_audit_test-{}", std::process::id()));
        let _ = fs::create_dir_all(&tmp_dir);
        let audit = ModelManager::run_downloads_expert_audit(&tmp_dir);
        assert!(audit.contains("SUSI MODEL DOWNLOADING EXPERT AUDIT"));
        assert!(audit.contains("HARDWARE STATS"));
        assert!(audit.contains("STORAGE REPOSITORY STATS"));
        assert!(audit.contains("NETWORK STATS"));
        assert!(audit.contains("BACKGROUND DOWNLOAD CONTROLLER STATS"));
        let _ = fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn foreground_install_rejects_non_https_sources() {
        let error =
            ModelManager::install_model_foreground("http://example.test/model.gguf").unwrap_err();
        assert!(error.contains("HTTPS"));
        assert!(ModelManager::install_model_foreground("not a URL").is_err());
    }

    /// Templates that ship a tools block must register; the plain
    /// messages-only loop must not.
    #[test]
    fn chat_template_tool_detection_covers_shipping_dialects() {
        for capable in [
            // qwen2.5-style: a `tools` JSON block plus <tool_call> delimiters
            "{% if tools is not none %}<|im_start|>tools{% endif %}{% for m in messages %}{{ m }}{% endfor %}<tool_call>",
            // llama-3.1-style: tools rendered into the system prompt
            "{{ bos_token }}{% if tools %}builtin tools{% endif %}{% for m in messages %}{% endfor %}",
            // mistral-style: available-tools section
            "[AVAILABLE_TOOLS]{{ tools }}[/AVAILABLE_TOOLS]{{ messages }}",
        ] {
            assert!(
                ModelManager::chat_template_supports_tools(capable),
                "tool-capable template must be detected"
            );
        }
        for plain in [
            "{% for m in messages %}{{ m.role }}{{ m.content }}{% endfor %}",
            "{{ bos_token }}{{ prompt }}",
            "",
        ] {
            assert!(
                !ModelManager::chat_template_supports_tools(plain),
                "tool-less template must not qualify"
            );
        }
    }

    #[test]
    fn supports_tool_calling_is_false_without_a_parseable_template() {
        assert!(!ModelManager::supports_tool_calling(Path::new(
            "/nonexistent/model.gguf"
        )));
    }
}
