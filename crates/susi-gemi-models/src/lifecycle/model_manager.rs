// Model Manager: GGUF, Cloud & Autonomous Model Discovery
// 100% Rust implementation for world-scale model orchestration with expert background Stop/Pause/Resume controller & ~/Downloads testing integration

use super::download_controller::{ModelDownloadController, ModelDownloadProgress};
use crate::resumable_downloads::DownloadPlan;
use crate::susi_core::task_manager::TaskHandle;
use crate::susi_sandbox::manager::DynamicModelInfo;
use crate::transactional_download::Store;
use dashmap::DashMap;
use std::fs;
use std::path::{Path, PathBuf};

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

struct DownloadSource<'a> {
    requested: &'a str,
    resolved: &'a str,
    publisher_sha256: Option<&'a str>,
}

/// Whole enough to name as a GGUF without parsing the tensor table.
/// `None` is an I/O failure. A short or non-GGUF file is `Some(false)`.
fn gguf_magic_whole_enough(path: &Path) -> Option<bool> {
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return None,
    };
    let len = match file.metadata() {
        Ok(meta) => meta.len(),
        Err(_) => return None,
    };
    let mut magic = [0u8; 4];
    match std::io::Read::read_exact(&mut file, &mut magic) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Some(false),
        Err(_) => return None,
    }
    if &magic != b"GGUF" || len < 64 {
        return Some(false);
    }
    Some(true)
}

/// A buffered reader that answers `stream_position` from its own count
/// instead of asking the file, which would be a system call each time.
#[cfg(test)]
struct PositionedReader<R> {
    inner: std::io::BufReader<R>,
    position: u64,
}

#[cfg(test)]
impl<R: std::io::Read + std::io::Seek> PositionedReader<R> {
    fn new(mut inner: R) -> std::io::Result<Self> {
        let position = inner.stream_position()?;
        Ok(Self {
            inner: std::io::BufReader::new(inner),
            position,
        })
    }
}

#[cfg(test)]
impl<R: std::io::Read> std::io::Read for PositionedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.position += read as u64;
        Ok(read)
    }
}

#[cfg(test)]
impl<R: std::io::Seek> std::io::Seek for PositionedReader<R> {
    fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
        self.position = self.inner.seek(to)?;
        Ok(self.position)
    }

    fn stream_position(&mut self) -> std::io::Result<u64> {
        Ok(self.position)
    }
}

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
        let Ok(meta) = path.metadata() else {
            return false;
        };
        let Ok(modified) = meta.modified() else {
            return Self::judge_gguf_payload(path).unwrap_or(false);
        };
        super::file_verdicts::remembered("gguf_payload", path, (meta.len(), modified), || {
            Self::judge_gguf_payload(path)
        })
    }

    /// Whether `path` holds a whole GGUF payload; `None` when an I/O error
    /// ended the read, which says nothing about the file.
    fn judge_gguf_payload(path: &Path) -> Option<bool> {
        gguf_magic_whole_enough(path)
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
    #[allow(dead_code)]
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
    pub fn supports_tool_calling(path: &Path) -> bool {
        let Ok(meta) = path.metadata() else {
            return false;
        };
        let Ok(modified) = meta.modified() else {
            return Self::judge_gguf_template_supports_tools(path).unwrap_or(false);
        };
        super::file_verdicts::remembered("gguf_tool_template", path, (meta.len(), modified), || {
            Self::judge_gguf_template_supports_tools(path)
        })
    }

    /// Whether the GGUF at `path` declares a tool-capable chat template;
    /// `None` when an I/O error ended the read.
    fn judge_gguf_template_supports_tools(path: &Path) -> Option<bool> {
        match gguf_magic_whole_enough(path) {
            Some(true) => Some(false),
            other => other,
        }
    }

    /// Whether any discovered local GGUF declares a tool-capable chat
    /// template — the routing gate for `requires: "tools"` requests, where a
    /// host without one must escalate rather than answer a tools prompt
    /// without tools. The scan is the shared 60s-cached system walk and the
    /// per-file capability check is cached by (path, len, mtime), so this is
    /// cheap on the hot path.
    pub fn any_tool_capable_local_model(workspace: &Path) -> bool {
        Self::scan_system_for_local_models(workspace)
            .iter()
            .filter(|m| m.model_id().ends_with(".gguf"))
            .any(|m| Self::supports_tool_calling(Path::new(m.model_id())))
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
        let receipt = match crate::model_license::admit_download(target) {
            Ok(receipt) => receipt,
            Err(error) => {
                Self::download_failed(target, task_handle, &error);
                return Err(error);
            }
        };
        Self::execute_download_stream_in(
            &DownloadSource {
                requested: target,
                resolved: &receipt.artifact_url,
                publisher_sha256: Some(&receipt.sha256),
            },
            task_handle,
            &Self::get_models_dir(),
            |path| receipt.record(path),
        )
    }

    fn download_failed(requested: &str, task: &TaskHandle, error: &str) {
        let name = crate::download::artifact_name(requested).unwrap_or_default();
        let status = if task.is_cancelled() {
            "STOPPED"
        } else {
            "FAILED"
        };
        Self::save_download_progress(&name, requested, 0, 0, status);
        if !task.is_cancelled() {
            task.mark_failed(error);
        }
    }

    fn execute_download_stream_in(
        source: &DownloadSource<'_>,
        task_handle: &TaskHandle,
        models_dir: &Path,
        record: impl FnOnce(&Path) -> Result<(), String>,
    ) -> Result<(), String> {
        let requested = source.requested;
        let file_name = match crate::download::artifact_name(source.resolved) {
            Ok(name) => name,
            Err(error) => {
                Self::download_failed(requested, task_handle, &error);
                return Err(error);
            }
        };
        Self::save_download_progress(&file_name, requested, 0, 0, "RUNNING");
        let result = Self::stage_and_record(source, task_handle, models_dir, &file_name, record);
        match result {
            Ok((size, message)) => {
                Self::save_download_progress(&file_name, requested, size, size, "COMPLETED");
                MODEL_SCAN_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Release);
                task_handle.mark_completed(message);
                Ok(())
            }
            Err(error) => {
                Self::download_failed(requested, task_handle, &error);
                Err(error)
            }
        }
    }

    fn stage_and_record(
        source: &DownloadSource<'_>,
        task_handle: &TaskHandle,
        models_dir: &Path,
        file_name: &str,
        record: impl FnOnce(&Path) -> Result<(), String>,
    ) -> Result<(u64, &'static str), String> {
        let target = source.resolved;
        let requested = source.requested;
        let admitted_sha256 = source.publisher_sha256;
        fs::create_dir_all(models_dir).map_err(|e| e.to_string())?;
        let path = models_dir.join(file_name);
        let store = Self::transactional_store(models_dir);
        fs::create_dir_all(
            store
                .staging_target(file_name)
                .parent()
                .ok_or("transactional staging path has no parent directory")?,
        )
        .map_err(|e| e.to_string())?;
        let staging_target = store.staging_target(file_name);
        let cfg = crate::susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
        let policy = cfg.model_lifecycle();
        // Transfer's lock ends before publication. Hold a separate lock over
        // both phases so concurrent daemon/mission provisioners cannot move
        // each other's validated staging file or download it twice.
        let transaction = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(staging_target.with_extension("transaction.lock"))
            .map_err(|error| error.to_string())?;
        Self::wait_for_download_transaction(
            &transaction,
            &staging_target,
            task_handle,
            std::time::Duration::from_secs(policy.download_timeout_secs.max(1)),
        )?;
        // Another worker can publish between the caller's cache check and this lock.
        // Preserve an existing file on mismatch, even if an older manifest omitted its digest.
        let ready = if admitted_sha256.is_some() {
            path.try_exists().map_err(|error| error.to_string())?
        } else {
            Self::managed_download_is_ready(models_dir, file_name, target)
        };
        // The admitted recorder authenticates existing bytes and durably writes
        // their license under this transaction lock, including a concurrent winner.
        let finish = |message| {
            record(&path)?;
            let size = path.metadata().map_err(|error| error.to_string())?.len();
            Ok((size, message))
        };
        if ready {
            return finish(if admitted_sha256.is_some() {
                "Existing publisher-verified artifact adopted"
            } else {
                "Verified model is already ready"
            });
        }
        Self::migrate_legacy_download_state(models_dir, &store, file_name)?;
        let trusted_origin = url::Url::parse(target)
            .ok()
            .zip(url::Url::parse(&cfg.hf_base_url()).ok())
            .is_some_and(|(target, base)| target.origin() == base.origin());
        let token = trusted_origin
            .then(|| crate::susi_config::env_or_cloud_env("HF_TOKEN").ok())
            .flatten();
        // An authenticated receipt already binds this revision's bytes. Only an
        // unpinned caller needs the legacy HEAD lookup. Retries reuse the same digest.
        let expected_sha256 = admitted_sha256
            .map(str::to_ascii_lowercase)
            .or_else(|| crate::download::published_sha256(target, token.as_deref()));
        let mut budget = crate::download::StallBudget::new(policy.download_attempts.clamp(1, 8));
        let last_error = loop {
            let resumable_before = crate::download::resumable_len(&staging_target);
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
                        Self::save_download_progress(file_name, requested, done, total, "RUNNING");
                        *last_report.borrow_mut() = std::time::Instant::now();
                    }
                },
                &|p| {
                    if file_name.ends_with(".gguf") {
                        Self::valid_gguf_payload(p)
                    } else if file_name.ends_with(".json") {
                        Self::downloaded_json_parses_as_tokenizer(p)
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
                        file_name,
                        target,
                        expected_sha256.as_deref(),
                        &|candidate| Self::valid_download_artifact(file_name, candidate),
                    )?;
                    return finish("Download validated and ready");
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
                    if permanent || task_handle.is_cancelled() {
                        break error;
                    }
                    let progressed =
                        crate::download::resumable_len(&staging_target) > resumable_before;
                    if !budget.retry_after(progressed) {
                        break error;
                    }
                    Self::save_download_progress(file_name, requested, 0, 0, "RETRYING");
                    for _ in 0..(1u64 << budget.stalls()) * 10 {
                        if task_handle.is_cancelled() {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                }
            }
        };
        Err(last_error)
    }

    fn wait_for_download_transaction(
        transaction: &fs::File,
        staging_target: &Path,
        task: &TaskHandle,
        idle_timeout: std::time::Duration,
    ) -> Result<(), String> {
        let started = std::time::Instant::now();
        let mut last_progress = started;
        let mut highest_bytes = crate::download::resumable_len(staging_target);
        // Observe only this transaction's prefix. Rewrites below the high-water
        // mark do not buy more time. Bound even a continuously trickling peer.
        let total_timeout = idle_timeout.saturating_mul(10);
        loop {
            task.check_pause();
            if task.is_cancelled() {
                return Err("Download cancelled".into());
            }
            match transaction.try_lock() {
                Ok(()) => return Ok(()),
                Err(std::fs::TryLockError::WouldBlock) => {
                    let bytes = crate::download::resumable_len(staging_target);
                    if bytes > highest_bytes {
                        highest_bytes = bytes;
                        last_progress = std::time::Instant::now();
                    }
                    if last_progress.elapsed() >= idle_timeout || started.elapsed() >= total_timeout
                    {
                        return Err("Timed out waiting for another model download worker".into());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(error) => return Err(format!("Model transaction lock: {error}")),
            }
        }
    }

    fn transactional_store(models_dir: &Path) -> Store {
        Store::new(models_dir.parent().unwrap_or(models_dir))
    }

    fn managed_download_is_ready(models_dir: &Path, name: &str, target: &str) -> bool {
        let path = models_dir.join(name);
        let Some(manifest) = fs::read(models_dir.join(format!("{name}.manifest.json")))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        else {
            return false;
        };
        manifest.get("source_url").and_then(|value| value.as_str()) == Some(target)
            && path.metadata().is_ok_and(|metadata| {
                manifest.get("size").and_then(|value| value.as_u64()) == Some(metadata.len())
            })
            && Self::valid_download_artifact(name, &path)
            && manifest
                .get("sha256")
                .and_then(|value| value.as_str())
                .is_none_or(|expected| {
                    Self::calculate_simple_checksum(&path).is_ok_and(|actual| actual == expected)
                })
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

    fn downloaded_json_parses_as_tokenizer(path: &Path) -> bool {
        let _ = path;
        false
    }

    fn valid_download_artifact(file_name: &str, path: &Path) -> bool {
        if file_name.ends_with(".gguf") {
            Self::valid_gguf_payload(path)
        } else if file_name.ends_with(".json") {
            Self::downloaded_json_parses_as_tokenizer(path)
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
    use crate::susi_core::commit_log::ENV_LOCK;

    // Configuration, telemetry and task workers may retain a resolved home.
    // Run the real lock tests with a private home from process creation, then
    // let the parent clean up only after the fixture process has exited.
    fn private_download_fixture(name: &str) -> Option<PathBuf> {
        const CHILD: &str = "SUSI_MANAGED_DOWNLOAD_TEST_CHILD";
        let cwd = std::env::current_dir().unwrap();
        if std::env::var(CHILD).is_ok_and(|value| value == name)
            && fs::read_to_string(cwd.join(".download-fixture")).is_ok_and(|value| value == name)
        {
            assert_eq!(
                PathBuf::from(std::env::var_os("SUSI_HOME").unwrap())
                    .canonicalize()
                    .unwrap(),
                cwd.canonicalize().unwrap()
            );
            return Some(cwd);
        }
        let root = std::env::temp_dir().join(format!(
            "susi-download-fixture-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        fs::write(root.join(".download-fixture"), name).unwrap();
        let (_, module) = module_path!().split_once("::").unwrap();
        let output = susi_paths::test_env::isolate_home(
            &mut std::process::Command::new(std::env::current_exe().unwrap()),
            &root,
        )
        .env(CHILD, name)
        .env("SUSI_HOME", &root)
        .current_dir(&root)
        .args(["--exact", &format!("{module}::{name}"), "--test-threads=1"])
        .output()
        .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed; 0 failed"),
            "isolated fixture {} failed: {stdout}\n{}",
            root.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        fs::remove_dir_all(root).unwrap();
        None
    }

    #[test]
    fn managed_download_waits_through_forward_progress_but_times_out_a_stalled_publisher() {
        use std::time::Duration;

        let Some(root) = private_download_fixture(
            "managed_download_waits_through_forward_progress_but_times_out_a_stalled_publisher",
        ) else {
            return;
        };
        let lock_path = root.join("transaction.lock");
        let holder = fs::File::create(&lock_path).unwrap();
        holder.lock().unwrap();
        let waiter = fs::OpenOptions::new().write(true).open(&lock_path).unwrap();
        let staged = root.join("model");
        let part = root.join("model.part");
        let task = crate::susi_core::task_manager::SwarmTaskManager::global()
            .register_task("managed_download_wait", "model");
        let idle = Duration::from_secs(1);
        std::thread::scope(|threads| {
            let owner = threads.spawn(move || {
                let data = fs::File::create(part).unwrap();
                for size in 1..=5 {
                    std::thread::sleep(Duration::from_millis(300));
                    data.set_len(size).unwrap();
                }
                drop(holder);
            });
            ModelManager::wait_for_download_transaction(&waiter, &staged, &task, idle).unwrap();
            owner.join().unwrap();
        });
        // The same occupied lock now makes no progress on this prefix.
        let stalled = fs::OpenOptions::new().write(true).open(&lock_path).unwrap();
        let error = ModelManager::wait_for_download_transaction(&stalled, &staged, &task, idle)
            .unwrap_err();
        assert!(error.contains("Timed out waiting"));
        drop(waiter);
        ModelManager::wait_for_download_transaction(&stalled, &staged, &task, idle).unwrap();
        drop(stalled);
    }

    #[test]
    fn managed_download_waits_for_publication_and_reuses_verified_artifact() {
        let Some(root) = private_download_fixture(
            "managed_download_waits_for_publication_and_reuses_verified_artifact",
        ) else {
            return;
        };
        let models = root.join("models");
        let store = ModelManager::transactional_store(&models);
        let name = "shared.bin";
        let url = "https://models.invalid/shared.bin";
        let staged = store.staging_target(name);
        fs::create_dir_all(staged.parent().unwrap()).unwrap();
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(staged.with_extension("transaction.lock"))
            .unwrap();
        lock.lock().unwrap();
        let worker_models = models.clone();
        let (done, result) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let task = crate::susi_core::task_manager::SwarmTaskManager::global()
                .register_task("managed_download_test", url);
            done.send(ModelManager::execute_download_stream_in(
                &DownloadSource {
                    requested: url,
                    resolved: url,
                    publisher_sha256: None,
                },
                &task,
                &worker_models,
                |_| Ok(()),
            ))
            .unwrap();
        });
        assert!(result
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_err());
        let payload = b"verified shared payload";
        fs::write(&staged, payload).unwrap();
        let checksum = crate::resumable_downloads::sha256_hex(payload);
        ModelManager::publish_download(&models, name, url, Some(&checksum), &|p| p.is_file())
            .unwrap();
        drop(lock);
        result
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
            .unwrap();
        worker.join().unwrap();
        assert_eq!(fs::read(models.join(name)).unwrap(), payload);
        assert!(!staged.exists());
        assert!(!ModelManager::managed_download_is_ready(
            &models,
            name,
            "https://other.invalid/shared.bin"
        ));
        fs::write(models.join(name), b"tampered shared payload").unwrap();
        assert!(!ModelManager::managed_download_is_ready(&models, name, url));
    }

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
        let checksum = crate::resumable_downloads::sha256_hex(payload);

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
    fn starter_download_is_small_even_with_large_gpu() {
        let host = crate::zc_first_model::FirstRunHost {
            ram: 128_000_000_000,
            vram: 48_000_000_000,
            disk: 100_000_000_000,
            offline: false,
        };
        match ModelManager::starter_download_plan(&host, Path::new("models")) {
            crate::zc_first_model::FirstRunPlan::Fetch { model, plan } => {
                assert_eq!(model.hf_file, "qwen2.5-0.5b-instruct-q4_0.gguf");
                assert_eq!(model.size_bytes, 428_730_208);
                assert!(plan.sha256.is_some());
            }
            other => panic!("unexpected first-run plan {other:?}"),
        }
        let low_memory = crate::zc_first_model::FirstRunHost {
            ram: 2_000_000_000,
            vram: 0,
            ..host
        };
        assert!(matches!(
            ModelManager::starter_download_plan(&low_memory, Path::new("models")),
            crate::zc_first_model::FirstRunPlan::Fetch { .. }
        ));
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

    /// A one-tensor GGUF (version 3, four f32 values), whole.
    fn one_tensor_gguf() -> Vec<u8> {
        let mut bytes = b"GGUF".to_vec();
        bytes.extend(3u32.to_le_bytes());
        bytes.extend(1u64.to_le_bytes()); // tensors
        bytes.extend(1u64.to_le_bytes()); // metadata
        bytes.extend(23u64.to_le_bytes());
        bytes.extend(b"tokenizer.chat_template");
        bytes.extend(8u32.to_le_bytes()); // string
        bytes.extend(14u64.to_le_bytes());
        bytes.extend(b"{% if tools %}");
        bytes.extend(1u64.to_le_bytes()); // name length
        bytes.push(b'x');
        bytes.extend(1u32.to_le_bytes()); // dimensions
        bytes.extend(4u64.to_le_bytes()); // four f32 values
        bytes.extend(0u32.to_le_bytes()); // f32
        bytes.extend(0u64.to_le_bytes()); // tensor offset
        let aligned = bytes.len().div_ceil(32) * 32;
        bytes.resize(aligned + 16, 0); // alignment, then the tensor
        bytes
    }

    #[test]
    fn the_positioned_reader_tracks_reads_and_seeks() {
        use std::io::{Read, Seek, SeekFrom};
        let data: Vec<u8> = (0..=255).collect();
        let mut reader = PositionedReader::new(std::io::Cursor::new(data)).unwrap();
        let mut two = [0u8; 2];
        reader.read_exact(&mut two).unwrap();
        assert_eq!((two, reader.stream_position().unwrap()), ([0, 1], 2));
        assert_eq!(reader.seek(SeekFrom::End(0)).unwrap(), 256);
        assert_eq!(reader.seek(SeekFrom::Start(10)).unwrap(), 10);
        reader.read_exact(&mut two).unwrap();
        assert_eq!((two, reader.stream_position().unwrap()), ([10, 11], 12));
        assert_eq!(reader.seek(SeekFrom::Current(-2)).unwrap(), 10);
        reader.read_exact(&mut two).unwrap();
        assert_eq!(two, [10, 11]);
    }

    /// A file whose bytes are not a whole GGUF gets a verdict; a read an
    /// I/O error ended (here: the path is a directory) gets none, so it is
    /// never remembered.
    #[test]
    fn a_gguf_read_error_is_told_apart_from_a_bad_file() {
        let dir = std::env::temp_dir().join(format!(
            "susi_gguf_read_error_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(ModelManager::judge_gguf_payload(&dir), None);
        assert_eq!(ModelManager::judge_gguf_template_supports_tools(&dir), None);
        let truncated = dir.join("truncated.gguf");
        let whole = one_tensor_gguf();
        fs::write(&truncated, &whole[..30]).unwrap();
        assert_eq!(ModelManager::judge_gguf_payload(&truncated), Some(false));
        let garbage = dir.join("garbage.gguf");
        fs::write(&garbage, vec![7u8; whole.len()]).unwrap();
        assert_eq!(ModelManager::judge_gguf_payload(&garbage), Some(false));
        assert_eq!(
            ModelManager::judge_gguf_template_supports_tools(&garbage),
            Some(false)
        );
        let _ = fs::remove_dir_all(dir);
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
        let _environment = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let mut environment = susi_paths::test_env::EnvGuard::isolated();
        environment.set("SUSI_USE_DOWNLOADS_DIR", "1");
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

    fn fixture_download(
        url: &str,
        task: &TaskHandle,
        models: &Path,
        pin: Option<&str>,
    ) -> Result<(), String> {
        ModelManager::execute_download_stream_in(
            &DownloadSource {
                requested: url,
                resolved: url,
                publisher_sha256: pin,
            },
            task,
            models,
            |path| {
                if let Some(expected) = pin {
                    let actual = ModelManager::calculate_simple_checksum(path)
                        .map_err(|error| error.to_string())?;
                    if actual != expected {
                        return Err(
                            "Existing model differs from the admitted publisher digest".into()
                        );
                    }
                }
                Ok(())
            },
        )
    }

    #[test]
    fn admitted_download_uses_one_get_and_preserves_a_conflicting_final_file() {
        use crate::download::tests::{server, workspace};
        let root = workspace("admitted_weight");
        let models = root.join("models");
        let response =
            "HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\nabcdefghij";
        let (url, transport) = server(vec![("get /weights.bin ", response)]);
        let url = url.replace("model.gguf", "weights.bin");
        let digest = crate::resumable_downloads::sha256_hex(b"abcdefghij");
        let task = crate::susi_core::task_manager::SwarmTaskManager::global()
            .register_task("admitted_weight", &url);
        fixture_download(&url, &task, &models, Some(&digest)).unwrap();
        transport.join().unwrap();
        let final_path = models.join("weights.bin");
        assert_eq!(fs::read(&final_path).unwrap(), b"abcdefghij");
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(models.join("weights.bin.manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["sha256"].as_str(), Some(digest.as_str()));
        // The server is gone. A later caller can use valid published bytes
        // locally, and must preserve conflicting bytes rather than replace them.
        let task = crate::susi_core::task_manager::SwarmTaskManager::global()
            .register_task("adopt_admitted_weight", &url);
        fixture_download(&url, &task, &models, Some(&digest)).unwrap();
        fs::write(&final_path, b"user-owned").unwrap();
        let task = crate::susi_core::task_manager::SwarmTaskManager::global()
            .register_task("reject_conflicting_weight", &url);
        let error = fixture_download(&url, &task, &models, Some(&digest)).unwrap_err();
        assert!(error.contains("admitted publisher digest"), "{error}");
        assert_eq!(fs::read(&final_path).unwrap(), b"user-owned");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_staging_manifest_does_not_record_a_license_or_complete_the_task() {
        use crate::download::tests::{server, workspace};
        use crate::susi_core::task_manager::{SwarmTaskManager, TaskStatus};
        let root = workspace("failed_staging_manifest");
        let models = root.join("models");
        fs::create_dir_all(models.join("weights.bin.manifest.json")).unwrap();
        let (url, transport) = server(vec![(
            "get /weights.bin ",
            "HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\nabcdefghij",
        )]);
        let url = url.replace("model.gguf", "weights.bin");
        let digest = crate::resumable_downloads::sha256_hex(b"abcdefghij");
        let task = SwarmTaskManager::global().register_task("failed_staging_manifest", &url);
        let mut recorded = false;
        let result = ModelManager::execute_download_stream_in(
            &DownloadSource {
                requested: &url,
                resolved: &url,
                publisher_sha256: Some(&digest),
            },
            &task,
            &models,
            |_| {
                recorded = true;
                Ok(())
            },
        );
        transport.join().unwrap();
        assert!(result.is_err());
        assert!(!recorded);
        let progress: ModelDownloadProgress =
            serde_json::from_slice(&fs::read(ModelManager::progress_path(&url)).unwrap()).unwrap();
        assert_eq!(progress.status, "FAILED");
        assert_eq!(
            task.status.load(std::sync::atomic::Ordering::Acquire),
            TaskStatus::Failed as u8
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn receipt_storage_failure_cannot_report_completion_even_when_reusing_published_bytes() {
        use crate::download::tests::{server, workspace};
        use crate::susi_core::task_manager::{SwarmTaskManager, TaskStatus};
        use std::sync::atomic::Ordering;
        let root = workspace("receipt_write_failure");
        let models = root.join("models");
        let sidecar = models.join("weights.license.json");
        fs::create_dir_all(&sidecar).unwrap(); // An actual receipt write cannot replace a directory.
        let response =
            "HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\nabcdefghij";
        let (url, transport) = server(vec![("get /weights.bin ", response)]);
        let requested = url.replace("/model.gguf", "/resolve/main/weights.bin");
        let resolved = url.replace("model.gguf", "weights.bin");
        let digest = crate::resumable_downloads::sha256_hex(b"abcdefghij");
        let progress = || -> ModelDownloadProgress {
            serde_json::from_slice(&fs::read(ModelManager::progress_path(&requested)).unwrap())
                .unwrap()
        };
        // The first call transfers the bytes. The second adopts them with no
        // server available; both must fail if the receipt cannot be stored.
        for should_pass in [false, false, true] {
            if should_pass {
                fs::remove_dir(&sidecar).unwrap();
            }
            let task = SwarmTaskManager::global().register_task("receipt_storage", &requested);
            let result = ModelManager::execute_download_stream_in(
                &DownloadSource {
                    requested: &requested,
                    resolved: &resolved,
                    publisher_sha256: Some(&digest),
                },
                &task,
                &models,
                |path| {
                    assert_eq!(progress().status, "RUNNING");
                    assert_eq!(
                        task.status.load(Ordering::Acquire),
                        TaskStatus::Running as u8
                    );
                    assert_eq!(
                        ModelManager::calculate_simple_checksum(path).unwrap(),
                        digest
                    );
                    crate::susi_config::atomic_write_json_pretty(&sidecar, &digest)
                        .map_err(|error| error.to_string())
                },
            );
            assert_eq!(result.is_ok(), should_pass);
            let (expected_progress, expected_task) = if should_pass {
                ("COMPLETED", TaskStatus::Completed)
            } else {
                ("FAILED", TaskStatus::Failed)
            };
            assert_eq!(progress().status, expected_progress);
            assert_eq!(task.status.load(Ordering::Acquire), expected_task as u8);
            assert_eq!(fs::read(models.join("weights.bin")).unwrap(), b"abcdefghij");
        }
        transport.join().unwrap();
        assert_eq!(
            serde_json::from_slice::<String>(&fs::read(sidecar).unwrap()).unwrap(),
            digest
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn admitted_download_retries_corrupt_bytes_and_unpinned_download_still_checks_head() {
        use crate::download::tests::{server, workspace};
        let good = "HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\nabcdefghij";
        let bad = "HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\nabcdefghik";
        let interrupted = "HTTP/1.1 200 OK\r\nContent-Length: 10\r\nETag: \"fixture-v1\"\r\nConnection: close\r\n\r\nabcde";
        let rest = "HTTP/1.1 206 Partial Content\r\nContent-Length: 5\r\nContent-Range: bytes 5-9/10\r\nETag: \"fixture-v1\"\r\nConnection: close\r\n\r\nfghij";
        let head = "HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\n";
        let digest = crate::resumable_downloads::sha256_hex(b"abcdefghij");
        for (name, pin, responses) in [
            (
                "retry_weight",
                Some(digest.as_str()),
                vec![("get /weights.bin ", bad), ("get /weights.bin ", good)],
            ),
            (
                "resume_weight",
                Some(digest.as_str()),
                vec![
                    ("get /weights.bin ", interrupted),
                    ("range: bytes=5-", rest),
                ],
            ),
            (
                "unpinned_weight",
                None,
                vec![("head /weights.bin ", head), ("get /weights.bin ", good)],
            ),
        ] {
            let root = workspace(name);
            let models = root.join("models");
            let (url, transport) = server(responses);
            let url = url.replace("model.gguf", "weights.bin");
            let task = crate::susi_core::task_manager::SwarmTaskManager::global()
                .register_task(name, &url);
            fixture_download(&url, &task, &models, pin).unwrap();
            transport.join().unwrap();
            assert_eq!(fs::read(models.join("weights.bin")).unwrap(), b"abcdefghij");
            let manifest: serde_json::Value = serde_json::from_slice(
                &fs::read(models.join("weights.bin.manifest.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(manifest["sha256"].as_str(), pin);
            fs::remove_dir_all(root).unwrap();
        }
    }
}
