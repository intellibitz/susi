// Model Manager: GGUF, Cloud & Autonomous Model Discovery
// 100% Rust implementation for world-scale model orchestration with expert background Stop/Pause/Resume controller & ~/Downloads testing integration

use super::download_controller::{ModelDownloadController, ModelDownloadProgress};
use crate::resumable_downloads::DownloadPlan;
use crate::susi_core::task_manager::TaskHandle;
use crate::susi_sandbox::manager::DynamicModelInfo;
use crate::transactional_download::Store;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

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

static MODEL_FAILURES: std::sync::OnceLock<RwLock<HashMap<String, std::time::Instant>>> =
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
        let failures =
            MODEL_FAILURES.get_or_init(|| std::sync::RwLock::new(std::collections::HashMap::new()));
        if success {
            failures
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .remove(model);
        } else {
            failures
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .insert(model.into(), std::time::Instant::now());
        }
    }

    pub(crate) fn cooling_down(model: &str) -> bool {
        let cooldown = crate::susi_sandbox::manager::SusiConfig::load_global()
            .unwrap_or_default()
            .model_lifecycle()
            .failure_cooldown_secs;
        MODEL_FAILURES
            .get_or_init(|| std::sync::RwLock::new(std::collections::HashMap::new()))
            .read()
            .unwrap_or_else(|e| e.into_inner())
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
        let parsed = susi_paths::weburl::WebUrl::parse(target)?;
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
            std::thread::sleep,
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

    // Inject only the retry clock, not transfer, validation, publication or
    // cancellation. Production always uses the real sleeper above.
    fn execute_download_stream_in(
        source: &DownloadSource<'_>,
        task_handle: &TaskHandle,
        models_dir: &Path,
        record: impl FnOnce(&Path) -> Result<(), String>,
        sleep: impl Fn(std::time::Duration),
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
        let result = Self::stage_and_record(source, task_handle, models_dir, record, sleep);
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
        record: impl FnOnce(&Path) -> Result<(), String>,
        sleep: impl Fn(std::time::Duration),
    ) -> Result<(u64, &'static str), String> {
        let target = source.resolved;
        let file_name = crate::download::artifact_name(target)?;
        let file_name = file_name.as_str();
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
        let trusted_origin = susi_paths::weburl::WebUrl::parse(target)
            .ok()
            .zip(susi_paths::weburl::WebUrl::parse(&cfg.hf_base_url()).ok())
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
                        sleep(std::time::Duration::from_millis(100));
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
#[path = "../tests/model_manager.rs"]
mod tests;
