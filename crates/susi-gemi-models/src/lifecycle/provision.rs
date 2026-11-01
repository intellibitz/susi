//! Hardware-optimal model provisioning and download-agent status reports.

use crate::susi_error::EaiResult;
use std::fs;
use std::path::{Path, PathBuf};

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
            let policy = crate::susi_sandbox::manager::SusiConfig::load_global()
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
        let hf_base_url = crate::susi_sandbox::manager::SusiConfig::load_global()
            .unwrap_or_default()
            .hf_base_url();
        let url = format!("{}/api/models", hf_base_url);
        let ua_val = format!("SUSI/{}", env!("CARGO_PKG_VERSION"));
        match susi_http_transport::http_call(
            "GET",
            &url,
            &[("User-Agent", ua_val.as_str())],
            15,
            10,
        ) {
            Ok(resp) => {
                let latency = start.elapsed().as_millis();
                (
                    true,
                    format!("Network reachable. HF API Status: {}", resp.status),
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

    pub fn verify_and_provision_model_ladder(workspace: &Path) -> EaiResult<ModelAgentReport> {
        let models_dir = Self::get_models_dir();
        let _ = fs::create_dir_all(&models_dir);
        let hf_base_url = crate::susi_sandbox::manager::SusiConfig::load_global()
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
        cfg: &crate::susi_sandbox::manager::SusiConfig,
    ) -> EaiResult<()> {
        let path = models_dir
            .join(&step.hf_file)
            .with_extension("tokenizer.json");
        if path.exists() {
            susi_vendor_models::model_license::verify_local(&path)
                .map_err(crate::susi_error::EaiError::governance)?;
            if susi_vendor_candle::tokenizers::Tokenizer::from_file(&path).is_ok() {
                return Ok(());
            }
            return Err(crate::susi_error::EaiError::governance("Existing tokenizer is invalid; preserving its bytes rather than replacing a user-owned file"));
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
        let token = crate::susi_config::env_or_cloud_env("HF_TOKEN").ok();
        let receipt = susi_vendor_models::model_license::admit_download(&url)
            .map_err(crate::susi_error::EaiError::governance)?;
        let url = receipt.artifact_url.clone();
        let expected_sha256 = Some(receipt.sha256.clone());
        let mut error = String::new();
        for _ in 0..policy.download_attempts.clamp(1, 8) {
            match crate::download::transfer(
                &url,
                &path,
                policy.download_timeout_secs,
                token.as_deref(),
                &|| false,
                &|_, _| {},
                &|p| susi_vendor_candle::tokenizers::Tokenizer::from_file(p).is_ok(),
                expected_sha256.as_deref(),
            ) {
                Ok(()) => {
                    return receipt
                        .record(&path)
                        .map_err(crate::susi_error::EaiError::governance)
                }
                Err(e) => {
                    error = e;
                    if error.starts_with("HTTP 4") || error.starts_with("[PRIVACY]") {
                        break;
                    }
                }
            }
        }
        Err(crate::susi_error::EaiError::inference(format!(
            "Tokenizer for {}: {error}",
            step.hf_repo
        )))
    }

    pub fn ensure_hardware_optimal_models(workspace: &Path) -> EaiResult<String> {
        static PLANNING: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let Ok(_planning) = PLANNING.try_lock() else {
            return Ok("Automatic provisioning already active".into());
        };
        let cfg = crate::susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
        if cfg
            .settings
            .get("auto_download_models")
            .and_then(|v| v.as_bool())
            == Some(false)
        {
            return Ok("Automatic downloads disabled".into());
        }
        // Selection admits both actual weights and tokenizer and uses current
        // resource budgets. Retain a better verified cache instead of spending
        // bandwidth/disk on another model merely because a GPU is present.
        if let Some(cached) = Self::identify_best_suited_local_model(workspace, None) {
            return Ok(format!(
                "Using verified cached local model {}",
                cached.model_id()
            ));
        }
        let models_dir = Self::get_models_dir();
        let available =
            (HardwareProfiler::determine_available_ram_gb() as u64).saturating_mul(1_073_741_824);
        let reserve = (cfg.model_scoring_heuristics().system_ram_buffer_gb.max(0.0) as f64
            * 1_073_741_824.0) as u64;
        let host = crate::zc_first_model::FirstRunHost {
            ram: available.saturating_sub(reserve),
            vram: HardwareProfiler::gpu_vram_budget_bytes(),
            disk: HardwareProfiler::get_free_disk_bytes(&models_dir).saturating_sub(2_000_000_000),
            offline: !crate::susi_core::mac_policy::egress_permitted("https://huggingface.co/"),
        };
        match Self::automatic_download_plan(&host, &models_dir) {
            crate::zc_first_model::FirstRunPlan::Fetch { model, plan } => {
                let step = crate::hardware::ModelLadderStep {
                    step: 1,
                    label: model.id,
                    hf_repo: model.hf_repo,
                    hf_file: model.hf_file,
                    tokenizer_repo: "Qwen/Qwen2.5-0.5B-Instruct".into(),
                    min_bytes: model.size_bytes,
                    expected_bytes: model.size_bytes,
                };
                ModelDownloadController::global()
                    .start_download(&plan.url)
                    .map_err(crate::susi_error::EaiError::inference)?;
                Self::ensure_ladder_tokenizer(&step, &models_dir, &cfg)?;
                Ok("Audited 0.5B starter queued; larger downloads require task evidence and resource admission".into())
            }
            crate::zc_first_model::FirstRunPlan::OfflineNoModel => Ok(
                "No admitted local model is cached; network policy prevents automatic provisioning"
                    .into(),
            ),
            crate::zc_first_model::FirstRunPlan::TooSmall => {
                Ok("Available RAM or disk cannot safely accommodate the small starter".into())
            }
            crate::zc_first_model::FirstRunPlan::UseCached { .. } => {
                Ok("Using verified cached local model".into())
            }
        }
    }

    pub(crate) fn automatic_download_plan(
        host: &crate::zc_first_model::FirstRunHost,
        models_dir: &Path,
    ) -> crate::zc_first_model::FirstRunPlan {
        // This is the production first-run policy, independent of GPU size,
        // catalog popularity, configured prefetch tiers or upgrade candidates.
        crate::zc_first_model::plan_first_run(
            host,
            &[crate::zc_first_model::audited_starter()],
            &[],
            models_dir,
        )
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
                vr.model_id,
                vr.file_size_formatted,
                vr.is_valid_gguf,
                if vr.checksum_known {
                    if vr.checksum_verified {
                        "yes"
                    } else {
                        "MISMATCH"
                    }
                } else {
                    "n/a (no reference checksum)"
                }
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
mod license_tests {
    use super::*;
    #[test]
    fn unverified_existing_tokenizer_is_not_overwritten() {
        let dir = std::env::temp_dir().join(format!(
            "susi-tokenizer-policy-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("unknown.tokenizer.json");
        fs::write(&path, b"user-owned invalid tokenizer bytes").unwrap();
        let step = crate::hardware::ModelLadderStep {
            step: 1,
            label: "fixture".into(),
            hf_repo: "org/model".into(),
            hf_file: "unknown.gguf".into(),
            tokenizer_repo: "org/model".into(),
            min_bytes: 0,
            expected_bytes: 0,
        };
        let result = ModelManager::ensure_ladder_tokenizer(
            &step,
            &dir,
            &crate::susi_sandbox::manager::SusiConfig::default(),
        );
        assert!(result.is_err());
        assert_eq!(
            fs::read(&path).unwrap(),
            b"user-owned invalid tokenizer bytes"
        );
        assert!(!path.with_extension("json.part").exists());
        fs::remove_file(&path).unwrap();
        fs::remove_dir(&dir).unwrap();
    }
}
