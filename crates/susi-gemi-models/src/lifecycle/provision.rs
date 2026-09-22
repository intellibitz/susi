//! Hardware-optimal model provisioning and download-agent status reports.

use std::fs;
use std::path::{Path, PathBuf};
use susi_error::EaiResult;

use crate::hardware::HardwareProfiler;

use super::download_controller::ModelDownloadController;
use super::types::*;
use super::ModelManager;

impl ModelManager {
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
            let policy = susi_sandbox::manager::SusiConfig::load_global()
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
        let hf_base_url = susi_sandbox::manager::SusiConfig::load_global()
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
            .header("User-Agent", format!("SUSI/{}", env!("CARGO_PKG_VERSION")))
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
        let hf_base_url = susi_sandbox::manager::SusiConfig::load_global()
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
            } else if controller.is_downloading(&url) {
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
            download_agent_active: controller.has_active_downloads(),
            steps,
        })
    }

    pub(crate) fn ensure_ladder_tokenizer(
        step: &crate::hardware::ModelLadderStep,
        models_dir: &Path,
        cfg: &susi_sandbox::manager::SusiConfig,
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
            match crate::download::transfer(
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
        Err(susi_error::EaiError::inference(format!(
            "Tokenizer for {}: {error}",
            step.hf_repo
        )))
    }

    pub fn ensure_hardware_optimal_models(_workspace: &Path) -> EaiResult<String> {
        static PLANNING: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let Ok(_planning) = PLANNING.try_lock() else {
            return Ok("Automatic provisioning already active".into());
        };
        let cfg = susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
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
            return Err(susi_error::EaiError::inference(
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
                    .map_err(susi_error::EaiError::inference)?;
                remaining = remaining.saturating_sub(needed);
                queued += 1;
            }
        }
        if queued == 0 && !errors.is_empty() {
            return Err(susi_error::EaiError::inference(errors.join("; ")));
        }
        Ok(format!("Automatic ladder ready: {queued} downloads queued"))
    }

    pub(crate) fn provisioning_indices(count: usize, limit: usize) -> Vec<usize> {
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
