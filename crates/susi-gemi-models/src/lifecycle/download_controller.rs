//! Background model download controller (pause / resume / stop).

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::fs;
use std::sync::Arc;
use susi_agents::task_manager::{SwarmTaskManager, TaskHandle};

use super::model_manager::ModelManager;

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
        let file_name = crate::download::artifact_name(&target)?;
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
            let limit = susi_sandbox::manager::SusiConfig::load_global()
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

    pub fn get_progress(&self, target: &str) -> Option<ModelDownloadProgress> {
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

    pub fn is_downloading(&self, target: &str) -> bool {
        self.active_downloads.contains_key(target)
    }

    pub fn has_active_downloads(&self) -> bool {
        !self.active_downloads.is_empty()
    }
}
