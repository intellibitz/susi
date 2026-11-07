//! Background model download controller (pause / resume / stop).

use crate::susi_core::task_manager::{SwarmTaskManager, TaskHandle};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::RwLock;

use super::model_manager::ModelManager;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelDownloadProgress {
    pub model_name: String,
    pub target_url: String,
    pub bytes_downloaded: u64,
    pub expected_bytes: u64,
    pub percentage: f32,
    pub status: String, // "RUNNING", "PAUSED", "COMPLETED", "STOPPED", "FAILED"
}

impl ModelDownloadProgress {
    /// Percentage of expected bytes downloaded, clamped to `0.0..=100.0`
    /// (`100.0` when `status == "COMPLETED"` and `expected_bytes == 0`, else `0.0`).
    #[must_use]
    pub fn computed_percentage(&self) -> f32 {
        if self.expected_bytes > 0 {
            ((self.bytes_downloaded as f64 / self.expected_bytes as f64) * 100.0).clamp(0.0, 100.0)
                as f32
        } else if self.status == "COMPLETED" {
            100.0
        } else {
            0.0
        }
    }

    /// True when the download has reached a terminal lifecycle state.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self.status.as_str(), "COMPLETED" | "STOPPED" | "FAILED")
    }
}

#[derive(Clone)]
pub struct ActiveDownloadTask {
    pub target_url: String,
    pub model_name: String,
    pub task_handle: Arc<TaskHandle>,
}

pub struct ModelDownloadController {
    active_downloads: RwLock<HashMap<String, ActiveDownloadTask>>,
}

impl Default for ModelDownloadController {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelDownloadController {
    #[must_use]
    pub fn new() -> Self {
        Self {
            active_downloads: RwLock::new(HashMap::new()),
        }
    }

    pub fn global() -> &'static Self {
        static CONTROLLER: std::sync::OnceLock<ModelDownloadController> =
            std::sync::OnceLock::new();
        CONTROLLER.get_or_init(Self::new)
    }

    pub fn start_download(&self, url: &str) -> Result<String, String> {
        let target = url.trim().to_string();
        let file_name = crate::download::artifact_name(&target)?;
        let mut guard = self
            .active_downloads
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let entry = match guard.entry(target.clone()) {
            std::collections::hash_map::Entry::Occupied(_) => {
                return Ok(format!("Download already active for: {}", target))
            }
            std::collections::hash_map::Entry::Vacant(entry) => entry,
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
            let limit = crate::susi_sandbox::manager::SusiConfig::load_global()
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
                .write()
                .unwrap_or_else(|e| e.into_inner())
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

    #[must_use]
    pub fn progress_from_file(progress_file: &Path, target: &str) -> Option<ModelDownloadProgress> {
        if let Ok(content) = fs::read_to_string(progress_file) {
            if let Ok(record) = serde_json::from_str::<ModelDownloadProgress>(&content) {
                if record.target_url == target || record.model_name == target {
                    return Some(record);
                }
            }
        }
        None
    }

    pub fn get_progress(&self, target: &str) -> Option<ModelDownloadProgress> {
        Self::progress_from_file(&ModelManager::progress_path(target), target)
    }

    pub fn list_active(&self) -> Vec<ModelDownloadProgress> {
        let mut list = Vec::new();
        for r in self
            .active_downloads
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
        {
            if let Some(prog) = self.get_progress(r.0) {
                list.push(prog);
            } else {
                list.push(ModelDownloadProgress {
                    model_name: r.1.model_name.clone(),
                    target_url: r.1.target_url.clone(),
                    bytes_downloaded: 0,
                    expected_bytes: 0,
                    percentage: 0.0,
                    status: "RUNNING".into(),
                });
            }
        }
        list
    }

    pub fn is_downloading(&self, target: &str) -> bool {
        self.active_downloads
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(target)
    }

    pub fn has_active_downloads(&self) -> bool {
        !self
            .active_downloads
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    }
}

#[cfg(test)]
// Test-only assertions: a failure here is the failure the test exists to report.
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn model_download_progress_percentage_terminal_and_serde_roundtrip() {
        let running = ModelDownloadProgress {
            model_name: "qwen3-0.6b.gguf".to_string(),
            target_url: "https://huggingface.co/Qwen/Qwen3-0.6B-GGUF/resolve/main/qwen3-0.6b.gguf"
                .to_string(),
            bytes_downloaded: 50,
            expected_bytes: 200,
            percentage: 25.0,
            status: "RUNNING".to_string(),
        };
        assert!((running.computed_percentage() - 25.0).abs() < f32::EPSILON);
        assert!(!running.is_terminal());

        let json = serde_json::to_string(&running).unwrap();
        let roundtrip: ModelDownloadProgress = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtrip, running);

        let zero_expected_running = ModelDownloadProgress {
            expected_bytes: 0,
            bytes_downloaded: 100,
            status: "RUNNING".to_string(),
            ..running.clone()
        };
        assert_eq!(zero_expected_running.computed_percentage(), 0.0);

        let zero_expected_completed = ModelDownloadProgress {
            expected_bytes: 0,
            bytes_downloaded: 0,
            status: "COMPLETED".to_string(),
            ..running.clone()
        };
        assert_eq!(zero_expected_completed.computed_percentage(), 100.0);
        assert!(zero_expected_completed.is_terminal());

        let over_reported = ModelDownloadProgress {
            bytes_downloaded: 500,
            expected_bytes: 200,
            status: "RUNNING".to_string(),
            ..running.clone()
        };
        assert_eq!(over_reported.computed_percentage(), 100.0);

        for (status, terminal) in [
            ("RUNNING", false),
            ("PAUSED", false),
            ("QUEUED", false),
            ("COMPLETED", true),
            ("STOPPED", true),
            ("FAILED", true),
        ] {
            let p = ModelDownloadProgress {
                status: status.to_string(),
                ..running.clone()
            };
            assert_eq!(
                p.is_terminal(),
                terminal,
                "unexpected is_terminal for {status}"
            );
        }
    }

    #[test]
    fn progress_from_file_matches_target_url_or_model_name_and_rejects_mismatch_or_corrupt_json() {
        let dir = std::env::temp_dir().join(format!(
            "susi-dl-ctrl-progress-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let file_path = dir.join("progress.json");
        assert_eq!(
            ModelDownloadController::progress_from_file(&file_path, "qwen3-0.6b.gguf"),
            None
        );

        let progress = ModelDownloadProgress {
            model_name: "qwen3-0.6b.gguf".to_string(),
            target_url: "https://huggingface.co/Qwen/Qwen3-0.6B-GGUF/resolve/main/qwen3-0.6b.gguf"
                .to_string(),
            bytes_downloaded: 1024,
            expected_bytes: 4096,
            percentage: 25.0,
            status: "RUNNING".to_string(),
        };
        fs::write(&file_path, serde_json::to_string(&progress).unwrap()).unwrap();

        assert_eq!(
            ModelDownloadController::progress_from_file(&file_path, &progress.target_url),
            Some(progress.clone())
        );
        assert_eq!(
            ModelDownloadController::progress_from_file(&file_path, &progress.model_name),
            Some(progress)
        );
        assert_eq!(
            ModelDownloadController::progress_from_file(&file_path, "unrelated.gguf"),
            None
        );

        fs::write(&file_path, "{corrupt json").unwrap();
        assert_eq!(
            ModelDownloadController::progress_from_file(&file_path, "qwen3-0.6b.gguf"),
            None
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn controller_rejects_invalid_urls_synchronously_and_tracks_active_downloads_without_duplicate_spawn(
    ) {
        let controller = ModelDownloadController::default();
        assert!(!controller.has_active_downloads());

        for bad_url in [
            "ftp://example.com/model.gguf",
            "https://example.com/",
            "https://example.com/..",
            "https://example.com/.",
        ] {
            assert!(
                controller.start_download(bad_url).is_err(),
                "expected invalid URL {bad_url} to fail synchronously"
            );
            assert!(!controller.has_active_downloads());
        }

        let target_url =
            "https://huggingface.co/Qwen/Qwen3-0.6B-GGUF/resolve/main/synthetic-unit-test.gguf"
                .to_string();
        let handle = SwarmTaskManager::global().register_task("model_download_test", &target_url);
        controller
            .active_downloads
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                target_url.clone(),
                ActiveDownloadTask {
                    target_url: target_url.clone(),
                    model_name: "synthetic-unit-test.gguf".to_string(),
                    task_handle: handle,
                },
            );

        assert!(controller.has_active_downloads());
        assert!(controller.is_downloading(&target_url));
        assert!(!controller.is_downloading("https://example.com/other.gguf"));

        let active = controller.list_active();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].model_name, "synthetic-unit-test.gguf");
        assert_eq!(active[0].target_url, target_url);
        assert_eq!(active[0].bytes_downloaded, 0);
        assert_eq!(active[0].status, "RUNNING");

        let dup_msg = controller.start_download(&target_url).unwrap();
        assert_eq!(
            dup_msg,
            format!("Download already active for: {target_url}")
        );
    }

    #[test]
    fn list_active_prefers_progress_file_when_present_and_stop_download_removes_active_entry() {
        // The progress file is written and read back under the data dir,
        // which the HOME-swapping tests in this crate change under
        // `ENV_LOCK`; a swap between the write and the read made the read
        // miss the file (0 bytes instead of 2048).
        let _env = crate::susi_core::commit_log::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let controller = ModelDownloadController::default();
        let model_name = format!("synthetic-progress-{}.gguf", std::process::id());
        let target_url =
            format!("https://huggingface.co/Qwen/Qwen3-0.6B-GGUF/resolve/main/{model_name}");
        let progress_path = ModelManager::progress_path(&target_url);
        let _ = fs::remove_file(&progress_path);

        let handle = SwarmTaskManager::global().register_task("model_download_live", &target_url);
        controller
            .active_downloads
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                target_url.clone(),
                ActiveDownloadTask {
                    target_url: target_url.clone(),
                    model_name: model_name.clone(),
                    task_handle: Arc::clone(&handle),
                },
            );

        // 1. Without a progress file on disk, list_active returns the 0-byte RUNNING placeholder.
        let placeholder = controller.list_active();
        assert_eq!(placeholder.len(), 1);
        assert_eq!(placeholder[0].bytes_downloaded, 0);
        assert_eq!(placeholder[0].status, "RUNNING");

        // 2. With a matching progress file on disk, list_active returns the live progress record.
        ModelManager::save_download_progress(&model_name, &target_url, 2_048, 4_096, "RUNNING");
        let live = controller.list_active();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].bytes_downloaded, 2_048);
        assert_eq!(live[0].expected_bytes, 4_096);
        assert!((live[0].percentage - 50.0).abs() < f32::EPSILON);

        // 3. Cancelling the handle and removing the active entry clears is_downloading.
        handle
            .cancel_flag
            .store(true, std::sync::atomic::Ordering::Release);
        assert!(handle.is_cancelled());
        controller
            .active_downloads
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&target_url);
        assert!(!controller.is_downloading(&target_url));
        assert!(!controller.has_active_downloads());
        assert!(controller.list_active().is_empty());

        let _ = fs::remove_file(&progress_path);
    }

    #[test]
    fn global_controller_returns_singleton_and_get_progress_returns_none_for_unknown_target() {
        let a = ModelDownloadController::global();
        let b = ModelDownloadController::global();
        assert!(std::ptr::eq(a, b));

        let local = ModelDownloadController::new();
        assert!(!local.has_active_downloads());
        assert!(local
            .get_progress(
                "https://huggingface.co/Qwen/Qwen3-0.6B-GGUF/resolve/main/absent-unit-test.gguf"
            )
            .is_none());
    }
}
